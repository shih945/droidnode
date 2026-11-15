#!/usr/bin/env python3
"""
Fetch Termux proot + libtalloc (aarch64) on a Windows host and stage them into
android/app/src/main/jniLibs/arm64-v8a/ as Android-loadable .so files.

Why this script exists: scripts/build-android.sh needs bash + dpkg + patchelf,
none of which exist on a plain Windows box. Everything here is stdlib-only.

What it produces:
  libproot.so          Termux proot, with DT_NEEDED patched
  libtalloc2x.so       libtalloc renamed to a valid Android .so name
  libproot_loader.so   proot's ELF loader (separate file, or extracted from proot)

The DT_NEEDED string "libtalloc.so.2" (14 chars) is rewritten in place to
"libtalloc2x.so" (14 chars) so the file length and the .dynstr layout are
untouched. The app already exports LD_LIBRARY_PATH=<nativeLibraryDir>, so the
linker resolves it from there — no RPATH patch is required.
"""

import hashlib
import io
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import time
import urllib.request

UA = {"User-Agent": "droidnode-build/1.0"}
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
JNI = os.path.join(ROOT, "android", "app", "src", "main", "jniLibs", "arm64-v8a")
WORK = os.path.join(ROOT, "build-cache")
CACHE = os.path.join(WORK, "proot-cache")
MANIFEST = os.path.join(CACHE, "manifest.json")

# The official pool intermittently answers with TLS EOF/502 from some networks;
# try mirrors in order and reuse what is already staged if all of them fail.
MIRRORS = [
    "https://packages.termux.dev/apt/termux-main",
    "https://mirrors.tuna.tsinghua.edu.cn/termux/apt/termux-main",
    "https://mirrors.ustc.edu.cn/termux/apt/termux-main",
]
POOL_PATHS = {
    "proot": "pool/main/p/proot/",
    "talloc": "pool/main/libt/libtalloc/",
    "shmem": "pool/main/liba/libandroid-shmem/",
}

# Older Termux builds do not depend on libandroid-shmem.so; newer ones (>= 5.1.107.100)
# do, and shipping that extra .so is more fragile on modern Android.
PREFERRED_PROOT = os.environ.get("PROOT_VERSION", "5.1.107.76")

NEEDED_OLD = b"libtalloc.so.2"
NEEDED_NEW = b"libtalloc2x.so"  # same length, still ends in ".so"

STAGED = ("libproot.so", "libtalloc2x.so", "libproot_loader.so")

# Allow forcing a re-download even when the staged files look valid.
FORCE_DOWNLOAD = os.environ.get("PROOT_FORCE_DOWNLOAD") == "1"


def log(msg):
    print(msg, flush=True)


def curl_fetch(url, timeout):
    """Windows ships curl, and it negotiates TLS with servers that reject
    Python's OpenSSL handshake (SSL: UNEXPECTED_EOF_WHILE_READING)."""
    curl = shutil.which("curl") or shutil.which("curl.exe")
    if not curl:
        raise RuntimeError("curl not found")
    proc = subprocess.run(
        [curl, "-fsSL", "--max-time", str(timeout), "-A", UA["User-Agent"], url],
        capture_output=True,
    )
    if proc.returncode != 0:
        detail = proc.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError("curl exit %d: %s" % (proc.returncode, detail[:200] or "no stderr"))
    return proc.stdout


def fetch(url, timeout=60, attempts=2):
    last = None
    for attempt in range(1, attempts + 1):
        try:
            return curl_fetch(url, timeout)
        except Exception as exc:  # noqa: BLE001 - urllib fallback below
            last = exc
        try:
            req = urllib.request.Request(url, headers=UA)
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return resp.read()
        except Exception as exc:  # noqa: BLE001 - retried below
            last = exc
            if attempt < attempts:
                delay = 1.5 * attempt
                log("  %s (retry %d/%d in %.1fs)" % (exc, attempt, attempts, delay))
                time.sleep(delay)
    raise last


def pick_deb(pool_key, name_pattern, preferred=None):
    """Try each mirror until one serves the pool index and a matching .deb."""
    path = POOL_PATHS[pool_key]
    last = None
    for base in MIRRORS:
        pool_url = base + "/" + path
        try:
            html = fetch(pool_url).decode("utf-8", "replace")
        except Exception as exc:  # noqa: BLE001 - try the next mirror
            log("  mirror unavailable: %s (%s)" % (base, exc))
            last = exc
            continue
        names = sorted(set(re.findall(r'href="([^"]+?\.deb)"', html)))
        hits = [n for n in names if re.match(name_pattern, n)]
        if not hits:
            log("  no matching package at %s" % pool_url)
            continue
        chosen = None
        if preferred:
            for n in hits:
                if preferred in n:
                    chosen = n
                    break
        if chosen is None:
            chosen = hits[-1]
        log("  picked %s (%s)" % (chosen, base))
        return pool_url + chosen, chosen
    raise SystemExit("no package matching %s on any mirror (%s)" % (name_pattern, last))


def staged_ok():
    """True when the three .so files exist, are ELF and proot is already patched."""
    for name in STAGED:
        path = os.path.join(JNI, name)
        try:
            if os.path.getsize(path) < 4096:
                return False
            with open(path, "rb") as fh:
                if fh.read(4) != b"\x7fELF":
                    return False
        except OSError:
            return False
    try:
        with open(os.path.join(JNI, "libproot.so"), "rb") as fh:
            return NEEDED_OLD not in fh.read()
    except OSError:
        return False


# ── local cache (build-cache/proot-cache) ──────────────────────────────────
#
# Every build reuses the staged .so files instead of hitting Termux. The
# manifest stores the size + sha256 of each file, so a truncated, edited or
# half-written cache is detected and replaced with a complete download.


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def cache_state():
    """Return 'ok', 'missing (...)' or 'modified (...)'.

    A modified cache is never repaired in place: the caller downloads the whole
    set again.
    """
    if not os.path.isfile(MANIFEST):
        return "missing (no manifest)"
    try:
        with open(MANIFEST, "r", encoding="utf-8") as fh:
            manifest = json.load(fh)
    except Exception as exc:  # noqa: BLE001 - corrupt manifest
        return "modified (bad manifest: %s)" % exc
    if manifest.get("preferred") != PREFERRED_PROOT:
        return "modified (proot version changed)"
    files = manifest.get("files") or {}
    for name in STAGED:
        path = os.path.join(CACHE, name)
        if not os.path.isfile(path):
            return "missing (%s)" % name
        try:
            size = os.path.getsize(path)
            if size != files.get(name, {}).get("size"):
                return "modified (%s size %d)" % (name, size)
            with open(path, "rb") as fh:
                if fh.read(4) != b"\x7fELF":
                    return "modified (%s is not ELF)" % name
            if sha256_file(path) != files.get(name, {}).get("sha256"):
                return "modified (%s checksum)" % name
        except OSError as exc:
            return "modified (%s: %s)" % (name, exc)
    try:
        with open(os.path.join(CACHE, "libproot.so"), "rb") as fh:
            if NEEDED_OLD in fh.read():
                return "modified (proot not patched)"
    except OSError as exc:
        return "modified (libproot.so: %s)" % exc
    return "ok"


def restore_from_cache():
    for name in os.listdir(CACHE):
        if name == os.path.basename(MANIFEST):
            continue
        if name not in STAGED and not name.startswith("libandroid-shmem"):
            continue
        with open(os.path.join(CACHE, name), "rb") as fh:
            data = fh.read()
        target = os.path.join(JNI, name)
        with open(target, "wb") as fh:
            fh.write(data)
        os.chmod(target, 0o755)
        log("  %-20s %9d bytes  (cache -> jniLibs)" % (name, len(data)))


def save_cache(targets):
    os.makedirs(CACHE, exist_ok=True)
    manifest = {"preferred": PREFERRED_PROOT, "mirrors": MIRRORS, "files": {}}
    for name, data in targets:
        path = os.path.join(CACHE, name)
        tmp = path + ".tmp"
        with open(tmp, "wb") as fh:
            fh.write(data)
        os.replace(tmp, path)
        os.chmod(path, 0o644)
        manifest["files"][name] = {"size": len(data), "sha256": sha256_bytes(data)}
    tmp = MANIFEST + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=2, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, MANIFEST)
    log("[cache] stored %d file(s) in %s" % (len(manifest["files"]), CACHE))


def ar_members(blob):
    """Yield (name, bytes) for each member of a Unix ar archive (.deb)."""
    if blob[:8] != b"!<arch>\n":
        raise SystemExit("not an ar archive")
    off = 8
    while off + 60 <= len(blob):
        hdr = blob[off:off + 60]
        name = hdr[0:16].decode("ascii", "replace").strip()
        try:
            size = int(hdr[48:58].decode("ascii").strip() or "0")
        except ValueError:
            break
        data = blob[off + 60:off + 60 + size]
        yield name.rstrip("/"), data
        off += 60 + size + (size & 1)


def deb_files(blob):
    """Yield (tar member name, bytes) for every regular file in a .deb payload."""
    payload = None
    for name, data in ar_members(blob):
        if name.startswith("data.tar"):
            payload = data
            break
    if payload is None:
        raise SystemExit("deb has no data.tar member")
    with tarfile.open(fileobj=io.BytesIO(payload), mode="r:*") as tf:
        for member in tf.getmembers():
            if not member.isfile():
                continue
            handle = tf.extractfile(member)
            if handle is None:
                continue
            yield member.name, handle.read()


# ── embedded loader extraction (mirrors scripts/build-android.sh) ────────────

ARCH_NAMES = {0xB7: "aarch64", 0x28: "arm32", 0x3E: "x86_64", 0x03: "x86"}


def elf_span(data, pos):
    if pos + 64 > len(data) or data[pos:pos + 4] != b"\x7fELF":
        return None
    ei_class, ei_data = data[pos + 4], data[pos + 5]
    if ei_data != 1 or ei_class not in (1, 2):
        return None
    try:
        if ei_class == 2:
            e_machine = struct.unpack_from("<H", data, pos + 18)[0]
            e_phoff = struct.unpack_from("<Q", data, pos + 32)[0]
            e_phentsz = struct.unpack_from("<H", data, pos + 54)[0]
            e_phnum = struct.unpack_from("<H", data, pos + 56)[0]
            e_shoff = struct.unpack_from("<Q", data, pos + 40)[0]
            e_shentsz = struct.unpack_from("<H", data, pos + 58)[0]
            e_shnum = struct.unpack_from("<H", data, pos + 60)[0]
            bits = 64
        else:
            e_machine = struct.unpack_from("<H", data, pos + 18)[0]
            e_phoff = struct.unpack_from("<I", data, pos + 28)[0]
            e_phentsz = struct.unpack_from("<H", data, pos + 42)[0]
            e_phnum = struct.unpack_from("<H", data, pos + 44)[0]
            e_shoff = struct.unpack_from("<I", data, pos + 32)[0]
            e_shentsz = struct.unpack_from("<H", data, pos + 46)[0]
            e_shnum = struct.unpack_from("<H", data, pos + 48)[0]
            bits = 32
    except struct.error:
        return None

    arch = ARCH_NAMES.get(e_machine, "0x%x" % e_machine)
    size = 0
    if e_shoff > 0 and e_shnum > 0:
        size = e_shoff + e_shentsz * e_shnum
    if size < 4096 and e_phoff > 0 and e_phnum > 0:
        max_end = 0
        for i in range(min(e_phnum, 64)):
            ph = pos + e_phoff + i * e_phentsz
            if ph + e_phentsz > len(data):
                break
            if bits == 64:
                p_type = struct.unpack_from("<I", data, ph)[0]
                p_offset = struct.unpack_from("<Q", data, ph + 8)[0]
                p_filesz = struct.unpack_from("<Q", data, ph + 32)[0]
            else:
                p_type = struct.unpack_from("<I", data, ph)[0]
                p_offset = struct.unpack_from("<I", data, ph + 4)[0]
                p_filesz = struct.unpack_from("<I", data, ph + 16)[0]
            if p_type == 1:
                max_end = max(max_end, p_offset + p_filesz)
        if max_end > 4096:
            size = (max_end + 4095) & ~4095
    if 4096 <= size <= 10 * 1024 * 1024 and pos + size <= len(data):
        return size, bits, arch
    return None


def extract_embedded_loader(proot_bytes):
    candidates = []
    pos = proot_bytes.find(b"\x7fELF", 4)
    while pos != -1:
        span = elf_span(proot_bytes, pos)
        if span:
            size, bits, arch = span
            log("  candidate ELF at %d: %d-bit %s, %d bytes" % (pos, bits, arch, size))
            candidates.append((pos, size, bits, arch))
        pos = proot_bytes.find(b"\x7fELF", pos + 4)
    for want_arch, want_bits in (("aarch64", 64), (None, 64), (None, None)):
        for pos, size, bits, arch in candidates:
            if (want_arch is None or arch == want_arch) and (want_bits is None or bits == want_bits):
                return proot_bytes[pos:pos + size]
    raise SystemExit("no embedded loader ELF found in proot")


def stage_from_network():
    os.makedirs(JNI, exist_ok=True)
    os.makedirs(WORK, exist_ok=True)

    log("[1/4] proot from Termux pool")
    proot_url, proot_name = pick_deb("proot", r"^proot_.*_aarch64\.deb$", PREFERRED_PROOT)
    proot_deb = fetch(proot_url)
    with open(os.path.join(WORK, proot_name), "wb") as fh:
        fh.write(proot_deb)

    proot_bin = None
    loader_bin = None
    for name, data in deb_files(proot_deb):
        base = os.path.basename(name)
        norm = name.replace("\\", "/")
        if base == "proot" and proot_bin is None:
            proot_bin = data
            log("  proot       <- %s (%d bytes)" % (name, len(data)))
        elif base == "loader" and "/proot/" in norm:
            # Termux >= 5.1.107.100 ships this at usr/libexec/proot/loader,
            # older builds used usr/lib/proot/loader.
            loader_bin = data
            log("  loader      <- %s (%d bytes)" % (name, len(data)))
    if proot_bin is None:
        raise SystemExit("no 'proot' binary inside the deb")

    log("[2/4] libtalloc from Termux pool")
    talloc_url, talloc_name = pick_deb("talloc", r"^libtalloc_.*_aarch64\.deb$")
    talloc_deb = fetch(talloc_url)
    with open(os.path.join(WORK, talloc_name), "wb") as fh:
        fh.write(talloc_deb)

    talloc_bin = None
    talloc_choice = None
    seen = []
    for name, data in deb_files(talloc_deb):
        base = os.path.basename(name)
        if not base.startswith("libtalloc.so"):
            continue
        seen.append("%s (%d bytes)" % (base, len(data)))
        # Packagers ship libtalloc.so.2 as a symlink to libtalloc.so.2.5.0;
        # tarfile only yields regular files, so keep the largest real payload.
        if talloc_bin is None or len(data) > len(talloc_bin):
            talloc_bin = data
            talloc_choice = name
    if seen:
        log("  candidates  : %s" % ", ".join(seen))
    if talloc_bin is None:
        raise SystemExit("no libtalloc.so* inside the deb")
    log("  libtalloc   <- %s (%d bytes)" % (talloc_choice, len(talloc_bin)))

    log("[3/4] patch DT_NEEDED %s -> %s" % (NEEDED_OLD.decode(), NEEDED_NEW.decode()))
    count = proot_bin.count(NEEDED_OLD)
    if count == 0:
        raise SystemExit("DT_NEEDED %r not found — proot layout changed" % NEEDED_OLD)
    proot_patched = proot_bin.replace(NEEDED_OLD, NEEDED_NEW)
    log("  patched %d occurrence(s)" % count)

    if loader_bin is None:
        log("  loader not shipped separately — extracting from proot")
        loader_bin = extract_embedded_loader(proot_bin)

    log("[4/4] stage into %s" % JNI)
    targets = [
        ("libproot.so", proot_patched),
        ("libtalloc2x.so", talloc_bin),
        ("libproot_loader.so", loader_bin),
    ]

    # Newer proot builds link against Termux's libandroid-shmem shim. Ship it
    # too, otherwise the Android linker refuses to start libproot.so.
    if b"libandroid-shmem.so" in proot_bin:
        log("  proot needs libandroid-shmem.so -> fetching")
        shmem_url, shmem_name = pick_deb("shmem", r"^libandroid-shmem_.*_aarch64\.deb$")
        shmem_deb = fetch(shmem_url)
        shmem_bin = None
        for name, data in deb_files(shmem_deb):
            if os.path.basename(name).startswith("libandroid-shmem.so"):
                shmem_bin = data
                log("  libandroid-shmem <- %s (%d bytes)" % (name, len(data)))
                break
        if shmem_bin is None:
            log("  !! could not find libandroid-shmem.so in %s" % shmem_name)
        else:
            targets.append(("libandroid-shmem.so", shmem_bin))
    else:
        log("  proot has no libandroid-shmem dependency")

    for filename, data in targets:
        path = os.path.join(JNI, filename)
        with open(path, "wb") as fh:
            fh.write(data)
        os.chmod(path, 0o755)
        log("  %-20s %9d bytes  %s" % (filename, len(data), path))

    save_cache(targets)
    log("done.")


def main():
    os.makedirs(JNI, exist_ok=True)
    os.makedirs(WORK, exist_ok=True)

    if FORCE_DOWNLOAD:
        log("[cache] PROOT_FORCE_DOWNLOAD=1 -> downloading a complete set")
    else:
        state = cache_state()
        if state == "ok":
            log("[cache] hit: %s" % CACHE)
            restore_from_cache()
            return
        log("[cache] %s -> downloading a complete set" % state)

    try:
        stage_from_network()
    except SystemExit as exc:
        if FORCE_DOWNLOAD or not staged_ok():
            raise
        log("!! proot download failed (%s)" % exc)
        log("   keeping the libraries already staged in %s" % JNI)
        log("   (set PROOT_FORCE_DOWNLOAD=1 to retry the download anyway)")
    except Exception as exc:  # noqa: BLE001 - network errors land here
        if FORCE_DOWNLOAD or not staged_ok():
            raise
        log("!! proot download failed: %s: %s" % (type(exc).__name__, exc))
        log("   keeping the libraries already staged in %s" % JNI)
        log("   (set PROOT_FORCE_DOWNLOAD=1 to retry the download anyway)")

if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # noqa: BLE001 - top-level report
        print("FAILED: %s: %s" % (type(exc).__name__, exc), file=sys.stderr)
        sys.exit(1)
