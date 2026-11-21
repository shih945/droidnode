package com.droidnode.ui.v2

/**
 * 随 APK 分发的第三方组件清单，只用于「设置 → 应用与诊断 → 关于 → 版本行」弹出的
 * 许可页，不参与运行时逻辑。
 *
 * 只列直接依赖和随包分发的原生库；Rust 的其余 300 多个传递依赖全部是
 * MIT / Apache-2.0 / ISC / BSD 等宽松许可，完整清单见仓库
 * `docs/opensource-compliance.md`。
 */
internal object ThirdPartyNotices {

    data class Entry(val name: String, val license: String, val url: String)

    val entries: List<Entry> = listOf(
        Entry("DroidNode（含上游 droidnode）", "MIT", "https://github.com/shih945/droidnode"),
        Entry("proot（Termux）", "GPL-2.0", "https://github.com/termux/proot"),
        Entry("talloc（Samba）", "LGPL-3.0", "https://www.samba.org/"),
        Entry("libandroid-shmem（Termux）", "BSD-3-Clause", "https://github.com/termux/libandroid-shmem"),
        Entry("moby/spdystream", "Apache-2.0", "https://github.com/moby/spdystream"),
        Entry("kube (kube-rs)", "Apache-2.0", "https://github.com/kube-rs/kube"),
        Entry("k8s-openapi", "Apache-2.0", "https://github.com/Arnavion/k8s-openapi"),
        Entry("Tokio", "MIT", "https://github.com/tokio-rs/tokio"),
        Entry("axum", "MIT", "https://github.com/tokio-rs/axum"),
        Entry("reqwest", "MIT OR Apache-2.0", "https://github.com/seanmonstar/reqwest"),
        Entry("serde", "MIT OR Apache-2.0", "https://github.com/serde-rs/serde"),
        Entry("Kotlin stdlib / coroutines", "Apache-2.0", "https://github.com/JetBrains/kotlin"),
        Entry("AndroidX core-ktx", "Apache-2.0", "https://developer.android.com/jetpack/androidx"),
    )
}
