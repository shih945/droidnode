package com.droidnode.ui.v2

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Single data source for the console pages. One shared poll loop feeds every
 * screen, instead of each Activity opening its own HTTPS requests on its own
 * timer. The loader is injectable so the merge logic is unit-testable.
 */
class NodeRepository(private val loader: Loader = DefaultLoader) {

    interface Loader {
        fun pods(): List<PodRow>?
        fun stats(): NodeStats?
        fun images(): List<ImageRow>?
        fun health(): Boolean
    }

    data class State(
        val pods: List<PodRow> = emptyList(),
        val stats: NodeStats? = null,
        val images: List<ImageRow> = emptyList(),
        val health: Boolean = false,
        /** Number of completed refreshes; 0 means "nothing loaded yet". */
        val ticks: Long = 0L,
    )

    private val _state = MutableStateFlow(State())
    val state: StateFlow<State> = _state.asStateFlow()

    private var job: Job? = null
    private var subscribers = 0
    private var tick = 0L

    /** Ref-counted: the first visible page starts the loop, the last one stops it. */
    @Synchronized
    fun acquire(scope: CoroutineScope) {
        subscribers++
        if (job == null) {
            job = scope.launch {
                while (isActive) {
                    refresh()
                    delay(POLL_MS)
                }
            }
        }
    }

    @Synchronized
    fun release() {
        subscribers = (subscribers - 1).coerceAtLeast(0)
        if (subscribers == 0) {
            job?.cancel()
            job = null
        }
    }

    /**
     * Fetch one snapshot. Failed requests keep the previous value (a temporary
     * agent hiccup must not blank the screen); `health` always reflects the
     * latest probe.
     *
     * The exception is a *dead* agent: when the health probe fails there is
     * nothing behind the numbers, so the lists are cleared. Keeping them made
     * the Pod page show pods that the cluster had already deleted, with every
     * row still saying "运行中".
     */
    suspend fun refresh() {
        val pods = withContext(Dispatchers.IO) { loader.pods() }
        val stats = withContext(Dispatchers.IO) { loader.stats() }
        val health = withContext(Dispatchers.IO) { loader.health() }
        tick++
        val images = if (tick % IMAGE_EVERY_TICKS == 0L) {
            withContext(Dispatchers.IO) { loader.images() }
        } else {
            null
        }
        _state.update { prev ->
            State(
                pods = if (health) pods ?: prev.pods else emptyList(),
                stats = if (health) stats ?: prev.stats else null,
                images = if (health) images ?: prev.images else emptyList(),
                health = health,
                ticks = tick,
            )
        }
    }

    /** Production loader backed by the kubelet API. */
    object DefaultLoader : Loader {
        override fun pods(): List<PodRow>? = Snapshot.podsWithStats()
        override fun stats(): NodeStats? = Snapshot.stats()
        override fun images(): List<ImageRow>? = Snapshot.images()
        override fun health(): Boolean = Snapshot.health()
    }

    companion object {
        const val POLL_MS = 2000L

        /** Images change slowly; refresh them every 3rd tick (~6s). */
        const val IMAGE_EVERY_TICKS = 3L
    }
}

/** Process-wide repository shared by the node, pods and images pages. */
val Repository = NodeRepository()
