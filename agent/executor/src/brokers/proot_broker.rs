use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::process::{Child, Command};
use tracing::{info, instrument, warn};

use crate::error::{DroidError, Result};
use crate::models::{ContainerLimits, Mount};

// ─── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait ProotBroker: Send + Sync {
    async fn execute(
        &self,
        rootfs: &Path,
        command: &[String],
        env: &[(String, String)],
        mounts: &[Mount],
        cwd: Option<&str>,
    ) -> Result<Child>;

    /// Same as [`execute`], but lets the caller request fake root (`proot -0`).
    ///
    /// Kubernetes containers run as uid 0 unless `securityContext` says otherwise,
    /// and plenty of images (nginx, postgres, ...) actually depend on that. Without
    /// `-0` they see the Android app uid and fail on their own files.
    // 参数是 proot 启动所需的完整契约：rootfs、命令、环境、挂载、工作目录、
    // 是否为 root、资源限制；拆成结构体会让每个实现都多一层解包。
    #[allow(clippy::too_many_arguments)]
    async fn execute_as(
        &self,
        rootfs: &Path,
        command: &[String],
        env: &[(String, String)],
        mounts: &[Mount],
        cwd: Option<&str>,
        fake_root: bool,
        limits: &ContainerLimits,
    ) -> Result<Child>;

    /// Build (but do not spawn) a proot command for `kubectl exec`.
    ///
    /// The caller owns stdio (pipes or a PTY slave) and process-group
    /// management. Kept separate from `execute_as` so exec does not inherit
    /// the container's RLIMIT_AS or stdout log piping.
    fn build_command(
        &self,
        rootfs: &Path,
        command: &[String],
        env: &[(String, String)],
        mounts: &[Mount],
        cwd: Option<&str>,
        fake_root: bool,
    ) -> Result<Command>;
}

// ─── Implementation ───────────────────────────────────────────────────────────

pub struct ProotBrokerImpl {
    proot_path: PathBuf,
}

impl ProotBrokerImpl {
    pub fn new(proot_path: PathBuf) -> Self {
        Self { proot_path }
    }
}

#[async_trait]
impl ProotBroker for ProotBrokerImpl {
    #[instrument(skip(self, env, mounts), fields(
        rootfs = %rootfs.display(),
        command = ?command
    ))]
    async fn execute(
        &self,
        rootfs: &Path,
        command: &[String],
        env: &[(String, String)],
        mounts: &[Mount],
        cwd: Option<&str>,
    ) -> Result<Child> {
        self.execute_as(
            rootfs,
            command,
            env,
            mounts,
            cwd,
            false,
            &ContainerLimits::default(),
        )
        .await
    }

    // 参数是 proot 启动所需的完整契约：rootfs、命令、环境、挂载、工作目录、
    // 是否为 root、资源限制；拆成结构体会让每个实现都多一层解包。
    #[allow(clippy::too_many_arguments)]
    async fn execute_as(
        &self,
        rootfs: &Path,
        command: &[String],
        env: &[(String, String)],
        mounts: &[Mount],
        cwd: Option<&str>,
        fake_root: bool,
        limits: &ContainerLimits,
    ) -> Result<Child> {
        if command.is_empty() {
            return Err(DroidError::Process("command is empty".into()));
        }

        let guest_tmp = rootfs.join("tmp");
        tokio::fs::create_dir_all(&guest_tmp).await?;

        // PROOT_TMP_DIR must be outside the rootfs (proot must not translate its own
        // loader path) and on an executable filesystem. On Android, code_cache is exec-
        // able while filesDir is noexec on API 29+. The Kotlin layer sets TMPDIR to
        // code_cache/tmp, so inherit that; fall back to the OS temp dir on desktop Linux.
        let proot_tmp = PathBuf::from(crate::config::get().tmpdir());
        tokio::fs::create_dir_all(&proot_tmp).await?;

        ensure_workload_command_exists(rootfs, command)?;

        let mut cmd = Command::new(&self.proot_path);

        // Disable proot's seccomp acceleration — conflicts with host and Android kernel
        // seccomp filters. Pure ptrace mode is slower but works everywhere.
        cmd.env("PROOT_NO_SECCOMP", "1");
        cmd.env("PROOT_TMP_DIR", &proot_tmp);
        // TMPDIR must be the guest path (/tmp), not the host path — proot remaps it.
        cmd.env("TMPDIR", "/tmp");
        // Default PATH for the guest — Android's inherited PATH only has /system/bin etc.
        // Pod-specified env vars applied below will override this if the pod sets its own PATH.
        cmd.env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        );

        // On Android, untrusted_app cannot execve files from code_cache/tmp
        // (dalvikcache_data_file type lacks `execute` SELinux permission on most vendor
        // kernels). proot's default behaviour — extract its embedded loader to PROOT_TMP_DIR
        // and then execve it — therefore silently fails; proot falls back to direct execve
        // which fails for musl ELFs (ld-musl-aarch64.so.1 is not on the Android host).
        //
        // Fix: ship the loader as libproot_loader.so in jniLibs. That directory has
        // nativelib_data_file SELinux type, which untrusted_app CAN execve. Tell proot
        // where the loader is via BOTH the env-var (PROOT_LOADER) and the CLI flag
        // (--loader) so that whichever mechanism the running proot version honours is used.
        let loader = self.proot_path.with_file_name("libproot_loader.so");
        let loader_found = loader.exists();
        if loader_found {
            // Termux proot does not accept a --loader CLI flag; use the env var instead.
            cmd.env("PROOT_LOADER", &loader);
        } else {
            warn!(
                loader = %loader.display(),
                "libproot_loader.so missing from nativeLibDir — musl ELF exec will fail on Android"
            );
        }

        info!(
            proot       = %self.proot_path.display(),
            rootfs      = %rootfs.display(),
            proot_tmp   = %proot_tmp.display(),
            loader      = %loader.display(),
            loader_found,
            command     = ?command,
            "spawning proot"
        );

        // Root filesystem
        cmd.args(["-r", rootfs.to_str().unwrap_or("/")]);
        cmd.args(["-w", cwd.unwrap_or("/")]);

        // Fake root. `-0` makes the guest see uid 0 while the host stays the
        // app uid, which is what most images expect (and what runAsUser=0 means).
        if fake_root {
            cmd.args(["-0"]);
        }

        // Default Linux pseudo-filesystems
        cmd.args(["-b", "/dev"]);
        cmd.args(["-b", "/proc"]);
        cmd.args(["-b", "/sys"]);

        // DNS config is generated per pod into the rootfs (`/etc/resolv.conf`
        // with the cluster DNS, `/etc/hosts` with hostAliases), so do NOT bind the
        // host's copies here - that used to shadow what we wrote and made
        // hostAliases and cluster DNS silently ineffective.
        if std::path::Path::new("/etc/resolv.conf").exists()
            && !rootfs.join("etc/resolv.conf").exists()
        {
            cmd.args(["-b", "/etc/resolv.conf"]);
        }

        // Additional mounts from the pod spec
        for mount in mounts {
            if !mount.source.exists() {
                warn!(
                    source = %mount.source.display(),
                    target = %mount.target.display(),
                    "skipping bind mount because host source does not exist"
                );
                continue;
            }

            let bind = format!("{}:{}", mount.source.display(), mount.target.display());
            cmd.args(["-b", &bind]);
        }

        // Environment variables
        for (key, val) in env {
            cmd.env(key, val);
        }

        cmd.args(command);

        // Capture stdout/stderr for log streaming
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // Keep stdin open so `kubectl attach` can write to the process.
        cmd.stdin(std::process::Stdio::piped());

        // Soft limits. Applied in the child between fork and exec so they
        // affect proot *and* everything it traces, without touching the agent.
        let nice = limits.nice_value();
        if !limits.is_empty() {
            info!(
                memory_bytes = ?limits.memory_bytes,
                cpu_millis = ?limits.cpu_millis,
                nice,
                "applying soft resource limits (RLIMIT_AS / nice, not cgroup)"
            );
        }

        // Put proot in its own process group so stop() can killpg() the whole
        // tracee tree. Without this, SIGKILLing proot leaves the container's
        // processes running (still holding their ports) - see the startup
        // orphan cleanup in main.rs.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.as_std_mut().process_group(0);

            if !limits.is_empty() {
                let memory = limits.memory_bytes;
                unsafe {
                    cmd.as_std_mut().pre_exec(move || {
                        if let Some(bytes) = memory {
                            let limit = libc::rlimit {
                                rlim_cur: bytes as libc::rlim_t,
                                rlim_max: bytes as libc::rlim_t,
                            };
                            if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                        }
                        if nice > 0 {
                            // Advisory only; a failure here must not block start.
                            let _ = libc::setpriority(libc::PRIO_PROCESS, 0, nice as libc::c_int);
                        }
                        Ok(())
                    });
                }
            }
        }

        let child = cmd
            .spawn()
            .map_err(|e| DroidError::Process(format!("proot spawn failed: {e}")))?;

        Ok(child)
    }

    fn build_command(
        &self,
        rootfs: &Path,
        command: &[String],
        env: &[(String, String)],
        mounts: &[Mount],
        cwd: Option<&str>,
        fake_root: bool,
    ) -> Result<Command> {
        if command.is_empty() {
            return Err(DroidError::Process("command is empty".into()));
        }

        std::fs::create_dir_all(rootfs.join("tmp"))?;
        let proot_tmp = PathBuf::from(crate::config::get().tmpdir());
        std::fs::create_dir_all(&proot_tmp)?;

        ensure_workload_command_exists(rootfs, command)?;

        let mut cmd = Command::new(&self.proot_path);
        cmd.env("PROOT_NO_SECCOMP", "1");
        cmd.env("PROOT_TMP_DIR", &proot_tmp);
        cmd.env("TMPDIR", "/tmp");
        cmd.env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        );

        let loader = self.proot_path.with_file_name("libproot_loader.so");
        if loader.exists() {
            cmd.env("PROOT_LOADER", &loader);
        }

        cmd.args(["-r", rootfs.to_str().unwrap_or("/")]);
        cmd.args(["-w", cwd.unwrap_or("/")]);
        if fake_root {
            cmd.args(["-0"]);
        }
        cmd.args(["-b", "/dev"]);
        cmd.args(["-b", "/proc"]);
        cmd.args(["-b", "/sys"]);

        if std::path::Path::new("/etc/resolv.conf").exists()
            && !rootfs.join("etc/resolv.conf").exists()
        {
            cmd.args(["-b", "/etc/resolv.conf"]);
        }

        for mount in mounts {
            if !mount.source.exists() {
                continue;
            }
            cmd.args([
                "-b",
                &format!("{}:{}", mount.source.display(), mount.target.display()),
            ]);
        }

        for (key, val) in env {
            cmd.env(key, val);
        }
        cmd.args(command);

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.as_std_mut().process_group(0);
        }

        Ok(cmd)
    }
}

fn ensure_workload_command_exists(rootfs: &Path, command: &[String]) -> Result<()> {
    let program = Path::new(&command[0]);

    if !program.is_absolute() {
        return Ok(());
    }

    let guest_program = program
        .strip_prefix("/")
        .map(|relative| rootfs.join(relative))
        .unwrap_or_else(|_| rootfs.join(program));

    let metadata = std::fs::symlink_metadata(&guest_program);
    match metadata {
        Ok(meta) => {
            let file_type = meta.file_type();
            info!(
                guest = %program.display(),
                host = %guest_program.display(),
                is_file = file_type.is_file(),
                is_symlink = file_type.is_symlink(),
                "workload command found in rootfs"
            );
            Ok(())
        }
        Err(e) => {
            let bin_entries = std::fs::read_dir(rootfs.join("bin"))
                .map(|entries| {
                    entries
                        .filter_map(|entry| entry.ok())
                        .take(20)
                        .filter_map(|entry| entry.file_name().into_string().ok())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_else(|_| "<missing bin dir>".to_string());

            Err(DroidError::Workload(format!(
                "workload command {} not found at {}: {e}; rootfs bin entries: {bin_entries}",
                program.display(),
                guest_program.display()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::{
        FilesystemBroker, FilesystemBrokerImpl, OciRegistryBroker, OciRegistryBrokerImpl,
    };
    use crate::services::foundation::image_pull_service::{ImagePullService, ImagePullServiceImpl};
    use crate::services::foundation::image_unpack_service::{
        ImageUnpackService, ImageUnpackServiceImpl,
    };
    use crate::services::orchestration::image_orchestration_service::{
        ImageOrchestrationService, ImageOrchestrationServiceImpl,
    };
    use std::sync::Arc;

    async fn prepare_alpine(tmp: &std::path::Path) -> std::path::PathBuf {
        let oci = Arc::new(OciRegistryBrokerImpl::new());
        let fs = Arc::new(FilesystemBrokerImpl::new(tmp.join("layers")));
        let pull = Arc::new(ImagePullServiceImpl::new(
            Arc::clone(&oci) as Arc<dyn OciRegistryBroker>,
            Arc::clone(&fs) as Arc<dyn FilesystemBroker>,
        ));
        let unpack = Arc::new(ImageUnpackServiceImpl::new(
            Arc::clone(&fs) as Arc<dyn FilesystemBroker>
        ));
        let svc = ImageOrchestrationServiceImpl::new(
            pull as Arc<dyn ImagePullService>,
            unpack as Arc<dyn ImageUnpackService>,
            tmp.join("rootfs"),
        );
        let image_ref = crate::models::ImageRef::parse("alpine:latest").unwrap();
        svc.prepare_image(&image_ref, crate::models::PullPolicy::IfNotPresent, None)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_proot_echo() {
        if std::env::var("DROIDNODE_SKIP_NETWORK_TESTS").is_ok() {
            eprintln!("skipping: registry access disabled via DROIDNODE_SKIP_NETWORK_TESTS");
            return;
        }

        let proot_path = std::path::PathBuf::from(
            std::env::var("PROOT_PATH").unwrap_or_else(|_| "/usr/bin/proot".into()),
        );
        if !proot_path.exists() {
            eprintln!("proot not found at {}, skipping", proot_path.display());
            return;
        }

        let tmp = std::env::temp_dir().join("droidnode_proot_test");
        let rootfs = prepare_alpine(&tmp).await;

        let broker = ProotBrokerImpl::new(proot_path);
        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo hello from proot".to_string(),
        ];
        let child = broker.execute(&rootfs, &cmd, &[], &[], None).await.unwrap();

        let output = child.wait_with_output().await.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        println!("stdout: {stdout}");
        println!("stderr: {stderr}");
        assert!(
            output.status.success(),
            "proot exited with {:?}\nstderr: {stderr}",
            output.status.code()
        );
        assert!(
            stdout.contains("hello from proot"),
            "unexpected stdout: {stdout}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn fake_proot_reports_exit_code_and_argv() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!("droidnode-proot-{}", std::process::id()));
        let rootfs = base.join("rootfs");
        std::fs::create_dir_all(rootfs.join("bin")).unwrap();
        std::fs::write(rootfs.join("bin/sh"), b"").unwrap();
        std::fs::set_permissions(
            rootfs.join("bin/sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let fake = base.join("proot");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\nexit 7\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let broker = ProotBrokerImpl::new(fake.clone());
        let mut child = broker
            .execute(
                &rootfs,
                &["/bin/sh".to_string(), "-c".to_string(), "true".to_string()],
                &[],
                &[],
                None,
            )
            .await
            .expect("spawn fake proot");
        let status = child.wait().await.unwrap();
        assert_eq!(status.code(), Some(7));

        let args = std::fs::read_to_string(format!("{}.args", fake.display())).unwrap();
        assert!(args.lines().any(|line| line == "-r"));
        assert!(args.contains(rootfs.to_str().unwrap()));

        let _ = std::fs::remove_dir_all(&base);
    }
}
