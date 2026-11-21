package com.droidnode.ui.v2

import android.content.Intent
import android.view.View
import android.widget.TextView

import com.droidnode.R

/** View binding and listener wiring for the settings screen. */
class SettingsBinder(private val a: SettingsActivity) {

    fun bind() {
        a.pillDot = a.findViewById(R.id.pillDot)
        a.pillText = a.findViewById(R.id.pillText)
        a.kubeStatus = a.findViewById(R.id.kubeStatus)
        a.kubeServer = a.findViewById(R.id.kubeServer)
        a.kubePath = a.findViewById(R.id.kubePath)
        a.kubeAuth = a.findViewById(R.id.kubeAuth)
        a.testHint = a.findViewById(R.id.testHint)
        a.editNodeName = a.findViewById(R.id.editNodeName)
        a.editPort = a.findViewById(R.id.editPort)
        a.editLogLevel = a.findViewById(R.id.editLogLevel)
        a.editMirrorDocker = a.findViewById(R.id.editMirrorDocker)
        a.editMirrorK8s = a.findViewById(R.id.editMirrorK8s)
        a.editMirrorQuay = a.findViewById(R.id.editMirrorQuay)
        a.configHint = a.findViewById(R.id.configHint)
        a.cfgRegistry = a.findViewById(R.id.cfgRegistry)
        a.batterySub = a.findViewById(R.id.batterySub)
        a.maintainSub = a.findViewById(R.id.maintainSub)
        a.agentReport = a.findViewById(R.id.agentReport)
        a.agentPowerSub = a.findViewById(R.id.agentPowerSub)
        a.btnStopAgent = a.findViewById(R.id.btnStopAgent)
        a.btnStartAgent = a.findViewById(R.id.btnStartAgent)
        a.switchMaintain = a.findViewById(R.id.switchMaintain)
        a.runState = a.findViewById(R.id.runState)
        a.btnRestartAgent = a.findViewById(R.id.btnRestartAgent)
        a.aboutVersion = a.findViewById(R.id.aboutVersion)
        // 版本行是开源许可二级页的入口
        a.findViewById<View>(R.id.aboutVersionRow).setOnClickListener {
            a.showPage(SettingsActivity.Page.LICENSES)
        }
        a.appearanceGroup = a.findViewById(R.id.appearanceGroup)
        a.langGroup = a.findViewById(R.id.langGroup)
        a.switchAutostart = a.findViewById(R.id.switchAutostart)
        a.switchKeepOnline = a.findViewById(R.id.switchKeepOnline)

        // 首页 + 五个二级页
        a.pill = a.findViewById(R.id.pill)
        a.settingsTitle = a.findViewById(R.id.settingsTitle)
        a.settingsBack = a.findViewById(R.id.settingsBack)
        a.settingsScroll = a.findViewById(R.id.settingsScroll)
        a.bottomNav = a.findViewById(R.id.bottomNav)
        a.pageHub = a.findViewById(R.id.pageHub)
        a.pageNode = a.findViewById(R.id.pageNode)
        a.pageCluster = a.findViewById(R.id.pageCluster)
        a.pageRuntime = a.findViewById(R.id.pageRuntime)
        a.pagePower = a.findViewById(R.id.pagePower)
        a.pageApp = a.findViewById(R.id.pageApp)
        a.pageLicenses = a.findViewById(R.id.pageLicenses)
        a.licensesList = a.findViewById(R.id.licensesList)
        a.hubNode = a.findViewById(R.id.hubNode)
        a.hubControl = a.findViewById(R.id.hubControl)
        a.hubState = a.findViewById(R.id.hubState)
        a.hubRestart = a.findViewById(R.id.hubRestart)
        a.hubStop = a.findViewById(R.id.hubStop)
        a.hubStart = a.findViewById(R.id.hubStart)
        a.hubEntries = a.findViewById(R.id.hubEntries)

        a.settingsBack.setOnClickListener { a.back() }
        a.hubRestart.setOnClickListener { a.askRestart() }
        a.hubStop.setOnClickListener { a.askStop() }
        a.hubStart.setOnClickListener { a.toggleAgent() }
        bindEntries(a)
        bindLicenses(a)
        a.ifaceGroup = a.findViewById(R.id.ifaceGroup)
        a.renderInterfaces()

        a.switchAutostart.isChecked = LocalConfig.autostart(a)
        a.switchAutostart.setOnCheckedChangeListener { _, checked ->
            LocalConfig.setAutostart(a, checked)
            Sheets.toast(a, if (checked) Tr.s("已开启开机自启") else Tr.s("已关闭开机自启"))
        }

        a.switchKeepOnline.isChecked = LocalConfig.keepOnline(a)
        a.switchKeepOnline.setOnCheckedChangeListener { _, checked ->
            LocalConfig.setKeepOnline(a, checked)
            // Takes effect on the next status tick: the service re-reads the pref
            // and either keeps holding the wake lock or starts the idle countdown.
            Sheets.toast(
                a,
                if (checked) Tr.s("锁屏时保持在线") else Tr.s("空闲 60 秒后会释放唤醒锁"),
            )
        }
        a.findViewById<View>(R.id.btnExport).setOnClickListener { a.exportDiagnostics() }

        a.appearanceGroup.setOnCheckedChangeListener { _, checkedId ->
            if (a.suppressAppearance) return@setOnCheckedChangeListener
            val mode = when (checkedId) {
                R.id.appearanceSystem -> LocalConfig.APPEARANCE_SYSTEM
                R.id.appearanceDark -> LocalConfig.APPEARANCE_DARK
                else -> LocalConfig.APPEARANCE_LIGHT
            }
            if (mode == LocalConfig.appearance(a)) return@setOnCheckedChangeListener
            LocalConfig.setAppearance(a, mode)
            a.recreate()
        }

        a.langGroup.setOnCheckedChangeListener { _, checkedId ->
            if (a.suppressAppearance) return@setOnCheckedChangeListener
            val tag = when (checkedId) {
                R.id.langZh -> LocalConfig.LANGUAGE_ZH
                R.id.langEn -> LocalConfig.LANGUAGE_EN
                else -> LocalConfig.LANGUAGE_SYSTEM
            }
            if (tag == LocalConfig.language(a)) return@setOnCheckedChangeListener
            LocalConfig.setLanguage(a, tag)
            a.recreate()
        }

        BottomNav.wire(a, BottomNav.Screen.SETTINGS)
        for (id in intArrayOf(R.id.tickPages, R.id.tickNode, R.id.tickLook, R.id.tickNet, R.id.tickLang, R.id.tickDiag, R.id.tickAbout)) {
            a.findViewById<PulseView>(id).apply {
                variant = PulseView.Variant.MINI
                setToneRes(R.color.text_3)
            }
        }

        a.findViewById<View>(R.id.btnJoin).setOnClickListener {
            a.startActivity(Intent(a, JoinActivity::class.java))
        }
        a.findViewById<View>(R.id.btnTest).setOnClickListener { a.testConnection() }
        a.findViewById<View>(R.id.btnSaveConfig).setOnClickListener { a.saveNodeConfig() }
        a.btnStopAgent.setOnClickListener { a.askStop() }
        a.btnStartAgent.setOnClickListener { a.toggleAgent() }
        a.findViewById<View>(R.id.btnRestartAgent).setOnClickListener { a.askRestart() }
        a.findViewById<View>(R.id.btnBattery).setOnClickListener { a.requestBatteryExemption() }
        a.switchMaintain.setOnCheckedChangeListener { _, checked ->
            if (!a.suppressSwitch) a.requestMaintenance(checked)
        }

        val version = try {
            a.packageManager.getPackageInfo(a.packageName, 0).versionName
        } catch (e: Exception) {
            "0.1.0"
        }
        a.aboutVersion.text = "DroidNode $version"

        a.ensureAgentStarted()
        a.render()
    }

    /** 首页五个入口：标题来自资源，副标题由 renderHub 填成当前值。 */
    /**
     * 开源许可二级页：静态列表，每行「名称 · 协议」+ 源码地址，点一行开浏览器。
     * 条目在 [ThirdPartyNotices]，不随运行时状态变化，所以 bind 时构建一次即可。
     */
    private fun bindLicenses(a: SettingsActivity) {
        for (entry in ThirdPartyNotices.entries) {
            val row = a.layoutInflater.inflate(
                R.layout.item_license,
                a.licensesList,
                false,
            ) as android.widget.LinearLayout
            row.findViewById<TextView>(R.id.licenseName).text = entry.name
            // 协议和地址放同一行，避免长名字把「· 协议」挤到下一行
            row.findViewById<TextView>(R.id.licenseUrl).text = "${entry.license} · ${entry.url}"
            row.setOnClickListener { openUrl(a, entry.url) }
            a.licensesList.addView(row)
        }
    }

    private fun openUrl(a: SettingsActivity, url: String) {
        try {
            a.startActivity(Intent(Intent.ACTION_VIEW, android.net.Uri.parse(url)))
        } catch (e: Exception) {
            Sheets.toast(a, Tr.s("没有可用的浏览器"))
        }
    }
    private fun bindEntries(a: SettingsActivity) {
        val entries = listOf(
            R.string.v2_settings_48 to SettingsActivity.Page.NODE,
            R.string.v2_settings_49 to SettingsActivity.Page.CLUSTER,
            R.string.v2_settings_50 to SettingsActivity.Page.RUNTIME,
            R.string.v2_settings_28 to SettingsActivity.Page.POWER,
            R.string.v2_settings_51 to SettingsActivity.Page.APP,
        )
        for ((titleRes, page) in entries) {
            val row = a.layoutInflater.inflate(R.layout.item_settings_entry, a.hubEntries, false)
            row.findViewById<TextView>(R.id.entryTitle).text = a.getString(titleRes)
            row.setOnClickListener { a.showPage(page) }
            a.hubEntries.addView(row)
        }
    }
}
