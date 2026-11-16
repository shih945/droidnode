package com.droidnode.data

import android.content.Context
import java.io.File

/**
 * Single root for everything the node owns on disk:
 *
 * ```
 * files/node/
 *   config/            kubeconfig, CAs, registry/network env
 *   rootfs/            unpacked images + rootfs/pods/<uid>/...
 *   layers/            compressed layer blobs
 *   logs/              container logs
 *   volumes/           local PV / emptyDir copies
 *   agent.sock         app <-> agent IPC socket
 * ```
 *
 * Everything lives under the app-private `filesDir`, so Android's
 * Settings -> App info -> Storage -> Clear data removes the whole node:
 * no kubeconfig, no CAs, no images, no pods, no logs, no settings. The APK's
 * native libraries (node-agent/proot) are not app data and survive, so a
 * cleared install starts again as a fresh, empty node.
 *
 * [migrateLegacy] moves the pre-`node/` layout (files/rootfs, files/kubeconfig,
 * …) into this tree exactly once. It only renames within the same filesystem,
 * so existing images and pods are preserved.
 */
object NodeStorage {
    private const val ROOT = "node"
    private const val CONFIG = "config"

    fun root(ctx: Context): File = File(ctx.filesDir, ROOT)

    fun config(ctx: Context): File = File(root(ctx), CONFIG)

    fun rootfs(ctx: Context): File = File(root(ctx), "rootfs")

    fun layers(ctx: Context): File = File(root(ctx), "layers")

    fun logs(ctx: Context): File = File(root(ctx), "logs")

    fun socket(ctx: Context): File = File(root(ctx), "agent.sock")

    fun kubeconfigRuntime(ctx: Context): File = File(config(ctx), "kubeconfig.runtime")

    fun kubeconfigEncrypted(ctx: Context): File = File(config(ctx), "kubeconfig.enc")

    fun legacyKubeconfig(ctx: Context): File = File(config(ctx), "kubeconfig")

    fun ensure(ctx: Context): File {
        val root = root(ctx)
        root.mkdirs()
        config(ctx).mkdirs()
        // Move the old trees BEFORE creating the new empty directories,
        // otherwise the destination exists and the rename is skipped.
        migrateLegacy(ctx)
        listOf(rootfs(ctx), layers(ctx), logs(ctx), File(root, "volumes"))
            .forEach { it.mkdirs() }
        return root
    }

    /**
     * Move the old flat layout into node/. Config files are renamed
     * individually; the three big trees are renamed as directories. A file
     * that already exists at the new location wins, so a half-finished
     * migration is safe to re-run.
     */
    fun migrateLegacy(ctx: Context) {
        val files = ctx.filesDir
        val config = config(ctx)
        val filesToMove = listOf(
            "kubeconfig.enc",
            "kubeconfig.runtime",
            "kubeconfig",
            "client-ca.crt",
            "kubelet-ca.crt",
            "kubelet-ca.key",
            "kubelet-ca.pub",
            "kubelet-serving.crt",
            "kubelet-serving.key",
            "kubelet-serving.ca",
            "registry-mirrors.env",
            "network-preference.env",
        )
        filesToMove.forEach { name ->
            val src = File(files, name)
            val dst = File(config, name)
            if (src.exists() && !dst.exists()) {
                if (!src.renameTo(dst)) {
                    // Never leave the node half-migrated: keep the old file if
                    // the rename failed, so the next start can retry.
                    return@forEach
                }
            }
        }
        listOf("rootfs", "layers", "logs").forEach { name ->
            val src = File(files, name)
            val dst = File(root(ctx), name)
            if (src.exists()) mergeDir(src, dst)
        }
        // A stale socket from a killed agent would make the next bind fail.
        File(files, "agent.sock").delete()
    }

    /**
     * Move [src] into [dst], merging directories that already exist. A target
     * that already exists wins (the new agent may have started writing before
     * the migration finished), so this is safe to re-run.
     */
    private fun mergeDir(src: File, dst: File) {
        if (!dst.exists() && src.renameTo(dst)) return
        dst.mkdirs()
        src.listFiles()?.forEach { child ->
            val target = File(dst, child.name)
            if (!target.exists()) {
                child.renameTo(target)
            } else if (child.isDirectory && target.isDirectory) {
                mergeDir(child, target)
            } else {
                // Target already has this entry (the new agent may have
                // unpacked/copied the same file); keep it and drop the legacy
                // copy so the old tree can be removed.
                child.delete()
            }
        }
        src.delete()
    }
}
