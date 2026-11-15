package com.droidnode.ui

import android.app.Activity
import android.content.Context
import android.content.res.Configuration
import android.os.Bundle
import android.view.View
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import com.droidnode.R
import com.droidnode.ui.v2.LocalConfig
import java.util.Locale

/**
 * 所有界面的基类：按「外观」设置选定配色，并给系统栏留出安全区。
 *
 * 颜色不是主题属性而是 @color 资源，所以外观切换靠 **覆盖 uiMode** 实现：
 * 覆盖后 values-night/colors.xml 会被选中，和真正的深色模式走同一条资源解析路径。
 * 主题（AppTheme / AppTheme.Night）只负责系统栏图标是深色还是浅色。
 *
 * targetSdk 35 在 Android 15 上强制 edge-to-edge，内容会画到状态栏下面；
 * 这里把 systemBars 的 inset 补成内边距，保证顶栏不与状态栏重叠。
 */
open class BaseActivity : Activity() {

    /** 本实例实际生效的外观；与偏好不一致时在 onResume 里重建。 */
    private var appliedMode: String? = null

    /** 本实例实际生效的语言；与偏好不一致时在 onResume 里重建。 */
    private var appliedLanguage: String? = null

    /**
     * 底栏的 5 个目的地：返回键直接退出应用（原生底栏语义），
     * 二级页（加入集群等）保持默认的「返回上一页」。
     */
    protected open val isTabRoot: Boolean get() = false

    override fun onBackPressed() {
        if (isTabRoot) {
            finishAffinity()
        } else {
            super.onBackPressed()
        }
    }

    override fun attachBaseContext(newBase: Context) {
        appliedMode = LocalConfig.appearance(newBase)
        appliedLanguage = LocalConfig.language(newBase)

        val config = Configuration(newBase.resources.configuration)
        var overridden = false

        // 外观：覆盖 uiMode 让 values-night/ 生效（主题只改系统栏图标）
        if (appliedMode != LocalConfig.APPEARANCE_SYSTEM) {
            val night = appliedMode == LocalConfig.APPEARANCE_DARK
            config.uiMode = (config.uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or
                (if (night) Configuration.UI_MODE_NIGHT_YES else Configuration.UI_MODE_NIGHT_NO)
            overridden = true
        }

        // 语言：空串表示跟随系统，此时不覆盖 locale
        val language = appliedLanguage.orEmpty()
        if (language.isNotEmpty()) {
            config.setLocale(Locale.forLanguageTag(language))
            overridden = true
        }

        if (!overridden) {
            super.attachBaseContext(newBase)
            return
        }
        super.attachBaseContext(newBase.createConfigurationContext(config))
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        // 代码里拼的文案通过 Tr.s 查资源，需要拿到带语言覆盖的 Activity 上下文
        com.droidnode.ui.v2.Tr.bind(this)
        val night = (resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) ==
            Configuration.UI_MODE_NIGHT_YES
        setTheme(if (night) R.style.AppTheme_Night else R.style.AppTheme)
        super.onCreate(savedInstanceState)
    }

    override fun onResume() {
        super.onResume()
        // 设置页改了外观/语言后，返回栈里的其他页面还停在旧配置 —— 自己重建一次。
        if (appliedMode != null && appliedMode != LocalConfig.appearance(this)) {
            recreate()
        } else if (appliedLanguage != null && appliedLanguage != LocalConfig.language(this)) {
            recreate()
        }
    }

    override fun setContentView(layoutResID: Int) {
        super.setContentView(layoutResID)
        padSystemBars()
    }

    override fun setContentView(view: View?) {
        super.setContentView(view)
        padSystemBars()
    }

    private fun padSystemBars() {
        val root = findViewById<View>(android.R.id.content) ?: return
        val left = root.paddingLeft
        val top = root.paddingTop
        val right = root.paddingRight
        val bottom = root.paddingBottom
        ViewCompat.setOnApplyWindowInsetsListener(root) { v, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
            v.setPadding(left, top + bars.top, right, bottom + bars.bottom)
            insets
        }
    }
}
