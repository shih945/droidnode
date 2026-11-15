package com.droidnode

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

data class LogLine(
    val timeMs: Long = System.currentTimeMillis(),
    val text: String,
)

object LogBuffer {
    private const val MAX = 500
    private val buf = ArrayDeque<LogLine>()
    private val _flow = MutableStateFlow<List<LogLine>>(emptyList())
    val flow: StateFlow<List<LogLine>> get() = _flow

    /**
     * CSI / SGR escape sequences, e.g. "\u001B[32m" or "\u001B[0;1m".
     * The ANSI colours are already disabled in the agent, but container output
     * (and logcat itself, which eats the ESC byte) still leaks fragments.
     */
    private val ansi = Regex("\u001B\\[[0-9;?]*[ -/]*[@-~]")

    /** Leftover "\u001B[32m" with the ESC already stripped: "[32m", "[0m", "[1;34m". */
    private val ansiOrphan = Regex("\\[[0-9]{1,2}(;[0-9]{1,2})*m")

    /** Other control chars that would render as boxes. */
    private val control = Regex("[\\u0000-\\u0008\\u000B\\u000C\\u000E-\\u001F\\u007F]")

    fun sanitize(text: String): String =
        control.replace(ansiOrphan.replace(ansi.replace(text, ""), ""), "")

    @Synchronized
    fun append(text: String) {
        val clean = sanitize(text)
        if (clean.isEmpty()) return
        while (buf.size >= MAX) buf.removeFirst()
        buf.addLast(LogLine(text = clean))
        _flow.value = buf.toList()
    }

    @Synchronized
    fun snapshot(): List<LogLine> = buf.toList()
}
