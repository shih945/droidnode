package com.droidnode.ui.v2

import com.droidnode.data.KubeConnectionTest

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.view.View
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import com.droidnode.R
import com.droidnode.brokers.ClusterBootstrapBroker
import com.droidnode.exposers.ForegroundServiceExposer
import com.droidnode.ui.BaseActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * 二级页：加入集群。三种方式显式可选（与原型一致）：
 *  1. 地址 + token —— CA 按 `k3s agent` 的做法从 /cacerts 自动获取（也可手填）；
 *  2. 导入控制面脚本生成的 kubeconfig 文件；
 *  3. 粘贴 kubeconfig 文本（含管理员证书，调试兜底）。
 *
 * 成功后才写盘并重启 agent —— 测试不通过不会覆盖现有配置。
 */
class JoinActivity : BaseActivity() {

    private enum class Mode { TOKEN, FILE, PASTE }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var mode = Mode.TOKEN

    private lateinit var panelToken: View
    private lateinit var panelText: View
    private lateinit var joinWarn: View
    private lateinit var pickFile: View
    private lateinit var rowToken: View
    private lateinit var rowFile: View
    private lateinit var rowPaste: View
    private lateinit var dotToken: View
    private lateinit var dotFile: View
    private lateinit var dotPaste: View
    private lateinit var joinServer: EditText
    private lateinit var joinToken: EditText
    private lateinit var joinCa: EditText
    private lateinit var joinText: EditText
    private lateinit var joinHint: TextView
    private lateinit var joinGo: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_join)

        panelToken = findViewById(R.id.panelToken)
        panelText = findViewById(R.id.panelText)
        joinWarn = findViewById(R.id.joinWarn)
        pickFile = findViewById(R.id.pickFile)
        rowToken = findViewById(R.id.joinModeToken)
        rowFile = findViewById(R.id.joinModeFile)
        rowPaste = findViewById(R.id.joinModePaste)
        dotToken = findViewById(R.id.joinDotToken)
        dotFile = findViewById(R.id.joinDotFile)
        dotPaste = findViewById(R.id.joinDotPaste)
        joinServer = findViewById(R.id.joinServer)
        joinToken = findViewById(R.id.joinToken)
        joinCa = findViewById(R.id.joinCa)
        joinText = findViewById(R.id.joinText)
        joinHint = findViewById(R.id.joinHint)
        joinGo = findViewById(R.id.joinGo)

        findViewById<View>(R.id.joinBack).setOnClickListener { finish() }
        rowToken.setOnClickListener { selectMode(Mode.TOKEN) }
        rowFile.setOnClickListener { selectMode(Mode.FILE) }
        rowPaste.setOnClickListener { selectMode(Mode.PASTE) }
        pickFile.setOnClickListener { pickKubeconfigFile() }
        joinGo.setOnClickListener { submit() }

        if (joinServer.text.isNullOrEmpty()) joinServer.setText("https://")
        for (id in intArrayOf(R.id.joinTickMode, R.id.joinTickCred)) {
            findViewById<PulseView>(id).apply {
                variant = PulseView.Variant.MINI
                setToneRes(R.color.text_3)
            }
        }
        renderMode()
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    private fun selectMode(next: Mode) {
        if (mode == next) return
        mode = next
        joinHint.visibility = View.GONE
        renderMode()
    }

    private fun renderMode() {
        val tokenMode = mode == Mode.TOKEN
        panelToken.visibility = if (tokenMode) View.VISIBLE else View.GONE
        panelText.visibility = if (tokenMode) View.GONE else View.VISIBLE
        joinWarn.visibility = if (mode == Mode.PASTE) View.VISIBLE else View.GONE

        rowToken.isActivated = tokenMode
        rowFile.isActivated = mode == Mode.FILE
        rowPaste.isActivated = mode == Mode.PASTE
        dotToken.isActivated = tokenMode
        dotFile.isActivated = mode == Mode.FILE
        dotPaste.isActivated = mode == Mode.PASTE

        joinGo.text = if (tokenMode) Tr.s("加入") else Tr.s("导入并加入")
    }

    private fun submit() {
        if (mode == Mode.TOKEN) submitToken() else submitText()
    }

    private fun submitToken() {
        val server = LocalConfig.normalizeServer(joinServer.text?.toString().orEmpty())
        // 复制长 JWT 时终端会自动换行，剪贴板里常带空格/换行；token 不允许空白。
        val token = joinToken.text?.toString().orEmpty().trim().filterNot { it.isWhitespace() }
        val ca = joinCa.text?.toString().orEmpty().trim()
        if (server.isEmpty() || server == "https:") {
            hint(Tr.s("填控制面地址，例如 https://192.0.2.10:6443"), false)
            return
        }
        if (token.isEmpty()) {
            hint(Tr.s("填 token：kubectl -n kube-system create token droidnode-agent"), false)
            return
        }
        busy(true)
        hint(if (ca.isBlank()) Tr.s("正在从 %1\$s/cacerts 获取 CA…", server) else Tr.s("正在验证控制面…"), true)

        scope.launch {
            var failure: String? = null
            val caPem: String? = if (ca.isNotBlank()) {
                ca
            } else {
                withContext(Dispatchers.IO) {
                    when (val r = ClusterBootstrapBroker.fetchCa(server)) {
                        is ClusterBootstrapBroker.Result.Ok -> r.bundle.pem
                        is ClusterBootstrapBroker.Result.Failed -> {
                            failure = r.message
                            null
                        }
                    }
                }
            }
            if (caPem.isNullOrBlank()) {
                busy(false)
                val reason = failure ?: Tr.s("未知错误")
                hint(Tr.s("取不到 CA：%1\$s；可在下方手填 CA", reason), false)
                return@launch
            }
            val fp = ClusterBootstrapBroker.fingerprint(caPem)
            if (fp.isNotEmpty()) Sheets.toast(this@JoinActivity, "CA SHA256 $fp")
            verifyAndSave(LocalConfig.buildKubeconfig(server, token, caPem))
        }
    }

    private fun submitText() {
        val text = joinText.text?.toString().orEmpty().trim()
        if (text.isEmpty()) {
            hint(Tr.s("先从文件选择，或粘贴 kubeconfig 全文"), false)
            return
        }
        if (LocalConfig.serverFrom(text) == null) {
            hint(Tr.s("没找到 server: 行，确认是完整的 kubeconfig"), false)
            return
        }
        busy(true)
        hint(Tr.s("正在验证控制面（TLS + 身份）…"), true)
        scope.launch { verifyAndSave(text) }
    }

    /** 先验证再落盘：不通过就保留原来的 kubeconfig，不会把节点弄断。 */
    private suspend fun verifyAndSave(kubeconfig: String) {
        val result = withContext(Dispatchers.IO) { KubeConnectionTest.test(kubeconfig) }
        val (ok, message) = ConnectionMessages.text(result)
        if (!ok) {
            busy(false)
            hint(message, false)
            return
        }
        if (!LocalConfig.writeKubeconfig(this, kubeconfig)) {
            busy(false)
            hint(Tr.s("写入失败，检查应用存储"), false)
            return
        }
        ForegroundServiceExposer.requestRestart(this)
        Sheets.toast(this, Tr.s("已加入 · %1\$s", message))
        setResult(RESULT_OK)
        finish()
    }

    private fun hint(message: String, ok: Boolean) {
        joinHint.visibility = View.VISIBLE
        joinHint.setTextColor(getColor(if (ok) R.color.text_2 else R.color.bad))
        joinHint.text = message
    }

    private fun busy(on: Boolean) {
        joinGo.isEnabled = !on
        joinGo.alpha = if (on) 0.5f else 1f
    }

    private fun pickKubeconfigFile() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
        }
        try {
            @Suppress("DEPRECATION")
            startActivityForResult(intent, REQ_PICK)
        } catch (e: Exception) {
            Sheets.toast(this, Tr.s("这台设备没有可用的文件选择器"))
        }
    }

    @Deprecated("startActivityForResult")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_PICK || resultCode != RESULT_OK) return
        val uri: Uri = data?.data ?: return
        val text = try {
            contentResolver.openInputStream(uri)?.bufferedReader()?.use { it.readText() }
        } catch (e: Exception) {
            null
        }
        if (text.isNullOrBlank()) {
            hint(Tr.s("读不到文件内容"), false)
            return
        }
        joinText.setText(text)
        if (mode != Mode.FILE) {
            mode = Mode.FILE
            renderMode()
        }
        hint(Tr.s("已读取文件（%1\$s 字节），点下方按钮导入", text.length), true)
    }

    private companion object {
        const val REQ_PICK = 2001
    }
}
