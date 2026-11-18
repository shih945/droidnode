package com.droidnode.ui.v2

import android.content.Intent
import android.view.View

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
        a.btnAgentPower = a.findViewById(R.id.btnAgentPower)
        a.switchMaintain = a.findViewById(R.id.switchMaintain)
        a.runState = a.findViewById(R.id.runState)
        a.btnRestartAgent = a.findViewById(R.id.btnRestartAgent)
        a.aboutVersion = a.findViewById(R.id.aboutVersion)
        a.appearanceGroup = a.findViewById(R.id.appearanceGroup)
        a.langGroup = a.findViewById(R.id.langGroup)
        a.switchAutostart = a.findViewById(R.id.switchAutostart)
        a.ifaceGroup = a.findViewById(R.id.ifaceGroup)
        a.renderInterfaces()

        a.switchAutostart.isChecked = LocalConfig.autostart(a)
        a.switchAutostart.setOnCheckedChangeListener { _, checked ->
            LocalConfig.setAutostart(a, checked)
            Sheets.toast(a, if (checked) Tr.s("已开启开机自启") else Tr.s("已关闭开机自启"))
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
        for (id in intArrayOf(R.id.tickNode, R.id.tickRun, R.id.tickPower, R.id.tickLook, R.id.tickNet, R.id.tickLang, R.id.tickDiag, R.id.tickAbout)) {
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
        a.findViewById<View>(R.id.btnAgentPower).setOnClickListener { a.toggleAgent() }
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
}
