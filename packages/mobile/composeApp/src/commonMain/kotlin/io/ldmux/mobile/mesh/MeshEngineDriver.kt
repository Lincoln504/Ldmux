package io.ldmux.mobile.mesh

import io.ldmux.mobile.appLog
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The screen's view of the radio, translated from [RadioEngine].
 *
 * **A translator, not a second protocol.** The engine (Rust, behind `ldmux_client_*`) owns the
 * handshake, the carrier, the transactions and the frames; this class turns the values it publishes
 * — [ClientEvent], [ClientMirror], [RadioStatus], [Route] — into the screen's own vocabulary,
 * [MeshTermView] and [MeshScreen]. It holds no byte, no retry and no clock of its own beyond the two
 * the caller hands it, because a second copy of any of those is `RC-2` waiting to happen.
 *
 * It exists for one reason: `MeshTermView` is the app's tested vocabulary for things that are still
 * true — `LinkDown(NeedsRadio)`, `Handshaking`, `HeldBy`, `NoShells`, `HostClosed` — and rewriting
 * 1,714 lines of screen and 2,952 lines of test to say the same things a different way would fix
 * nothing the operator sees. `docs/RADIO-PLAN.md` §7, `AR-12`, `AR-13`.
 *
 * **What it deliberately does not offer.** `flush`, `setHold`, `detach` and `tick` were the v14
 * batch model's: one command is one message in v15, there is no `Hold` message, and the engine owns
 * its own loop and deadline. Those members have no truthful implementation here, so they do not
 * exist. The screen's batch region is what S5b-1b-ii replaces.
 */
class MeshEngineDriver(
    private val engine: RadioEngine,
    /** Monotonic milliseconds, the same clock the engine's deadlines are expressed in. */
    private val clock: () -> Long,
    /** Wall milliseconds, for the handshake's timestamp. Only its distance from the host's matters. */
    private val wall: () -> Long = { 0L },
    /** The shell to open on connect, empty for "ask which are running". */
    private val selector: String = "",
    /**
     * The grid to state on the first page, when it is known before anything is measured.
     *
     * A resume opens from inside [run], before a surface exists, so without this the host paints at
     * the desk's size and repaints at the phone's: a dozen seconds of air for a picture the operator
     * never sees.
     */
    initialGrid: Pair<Int, Int>? = null,
    /**
     * Whether data records are metered at the band's allowance.
     *
     * **The allowance is the caller's, and it is enforced in the engine**: with `records_left == 0`
     * the transaction layer starts no new part. A caller that never pushes one leaves a radio in a
     * duty-cycled band transmitting outside its condition of use, so this is not a preference.
     */
    private val metered: Boolean = true,
) {
    private val _view = MutableStateFlow<MeshTermView>(MeshTermView.Idle)
    val view: StateFlow<MeshTermView> = _view.asStateFlow()

    /** How many times this link has been re-asked. The screen's "still trying" sentence. */
    private val _attempt = MutableStateFlow(0)
    val attempt: StateFlow<Int> = _attempt.asStateFlow()

    private val _shells = MutableStateFlow(MeshShells(emptyList(), null))
    val shells: StateFlow<MeshShells> = _shells.asStateFlow()

    private val _marks = MutableStateFlow<Map<String, ShellMark>>(emptyMap())
    val marks: StateFlow<Map<String, ShellMark>> = _marks.asStateFlow()

    /**
     * The host said the pane had stopped changing.
     *
     * Forwarded from [ClientEvent.Settled], so the screen can end a wait on the host's own word
     * rather than on a timer of its own.
     */
    var onIdle: (() -> Unit)? = null

    /** A page of the shell's own scrollback arrived. */
    var onHistoryPage: ((TermMessage.HistoryPage) -> Unit)? = null

    private var grid: Pair<Int, Int>? = initialGrid

    /** The page currently asked for: what [selector] said, or what `Created`/`open` named. */
    private var page: String = selector

    /** True from the moment a page is asked for until its first screen arrives. */
    internal var attaching: Boolean = false
        private set

    /** The screen applied last, so a repaint of the same one is skipped. */
    private var lastScreen: MeshScreen? = null

    /** The last frame counter the host sent; the mirror's identity. */
    private var lastSnapshot: Int = 0

    /** When a record last arrived from the host, for the "answered N s ago" line. */
    private var lastHeardAt: Long? = null

    /** When the last screen arrived, for the "quiet for N s" line. */
    private var lastScreenAt: Long? = null

    /** Entries of a `Sessions` answer being assembled across its pages. */
    private var listing: MutableList<SessionEntry>? = null

    private var closed = false

    /** The page is open and nothing has been painted: a page exists, its screen has not arrived. */
    val screenUnpainted: Boolean get() = attaching

    /** Repeaters on the route to the host, or null when no path is held. */
    fun hopsNow(): Int? = engine.route.value.hopsOrNull

    /** Milliseconds since a record last arrived from the host; null when nothing ever has. */
    fun answeredAgoMs(): Long? = lastHeardAt?.let { clock() - it }

    /** Milliseconds since the screen last changed; null when nothing ever has. */
    suspend fun quietForMs(): Long? = lastScreenAt?.let { clock() - it }

    /**
     * What the link has actually done, in one sentence.
     *
     * The engine keeps the three facts the sentence is made of — rungs spent, rungs spent while no
     * path was held, and whether the mesh answered one of those — so this says the same thing the
     * v14 driver said without re-deriving it from a second set of counters.
     */
    fun linkEffort(): String {
        val stats = lastStats ?: return "connecting"
        return linkEffortLine(
            stats.rungsUsed.toInt(),
            stats.blindAsks.toInt(),
            stats.routeAnswered,
        )
    }

    /** The engine's counters as of the last event, read without suspending. */
    private var lastStats: ClientStats? = null

    /**
     * Open the link, connect, and follow the engine's events until the caller cancels.
     *
     * **Nothing here retries.** The engine's bootstrap and its transaction layer own every retry and
     * every deadline; a second ladder in the app is what `RC-5` was. What this does is start the
     * link, ask for the page, and translate what comes back.
     */
    suspend fun run() {
        try {
            engine.start()
            meter()
            engine.connect(clock(), wall())
            attachIfPossible()
            engine.events.collect { event -> apply(event) }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Exception) {
            // The engine reports its own faults as `linkState`/`problem`; an exception here is the
            // loop itself failing, which the screen must see rather than swallow.
            appLog("mesh engine loop failed: ${e.message ?: e::class.simpleName}")
            _view.value = MeshTermView.LinkDown(MeshLinkState.SessionFailed, e.message)
        }
    }

    /** Begin the link without following events; the caller drives [onEvent] itself. */
    suspend fun start() {
        engine.start()
        meter()
    }

    /**
     * Push the hour's allowance to the engine.
     *
     * Called at the start and after every answer, because both facts it is computed from move: the
     * radio reports its spent airtime on every status poll, and a preset written by the settings
     * screen changes which band's rule applies. Pushing it once at connect would meter the first
     * minute and stop — which is the same silence as never pushing it at all, one step later.
     */
    suspend fun meter() {
        engine.setRecordsLeft(
            RadioMeter.recordsLeft(engine.radio.value, metered) ?: UNMETERED,
        )
    }

    /** Ask for [id]'s page, or ask which shells are running when it is empty. */
    private suspend fun attachIfPossible() {
        val (cols, rows) = grid ?: (0 to 0)
        if (page.isNotEmpty()) {
            attaching = true
            _view.value = MeshTermView.Handshaking
            val rc = engine.open(page, cols, rows, clock())
            if (rc != ClientCode.OK) {
                attaching = false
                _view.value = MeshTermView.LinkDown(MeshLinkState.Refused, ClientCode.word(rc))
            }
        } else {
            _view.value = MeshTermView.Listing
            engine.list(clock())
        }
    }

    /**
     * Apply one engine event to the screen's state.
     *
     * `internal` rather than private because the mapping is what the tests are about, and driving
     * `run` for one event would need a whole engine and a virtual clock. The loop calls this
     * function and nothing else, so the mapping under test is the mapping that ships.
     */
    internal suspend fun apply(event: ClientEvent) {
        lastHeardAt = clock()
        when (event) {
            is ClientEvent.Attached -> {
                page = event.shell
                attaching = true
                _shells.value = _shells.value.copy(current = event.shell)
                // The page is open; the screen itself arrives as a `Settled`.
                _view.value = MeshTermView.ProfileMissing
            }

            is ClientEvent.Settled -> {
                lastScreenAt = clock()
                attaching = false
                meter()
                val mirror = engine.mirror(event.shell) ?: return
                lastStats = engine.stats()
                val screen = screenOf(event.shell, mirror) ?: return
                lastSnapshot = event.snapshotId
                lastScreen = screen
                _shells.value = _shells.value.copy(current = event.shell)
                _view.value = MeshTermView.Live(
                    screen = screen,
                    absent = emptyList(),
                    budgetLeft = engine.radio.value.opsLeft,
                    stalled = false,
                    resyncing = false,
                    painted = true,
                )
                onIdle?.invoke()
            }

            is ClientEvent.Held -> {
                attaching = false
                _view.value = MeshTermView.HeldBy(event.shell)
            }

            is ClientEvent.Closed -> {
                attaching = false
                _view.value = MeshTermView.HostClosed(event.reason)
            }

            is ClientEvent.Created -> {
                // The answer to a create is this link's to act on, whatever the list was showing:
                // backing out of a failed list must not silently swallow the new shell's id.
                page = event.id
                attaching = true
                _view.value = MeshTermView.ProfileMissing
                val (cols, rows) = grid ?: (0 to 0)
                engine.open(event.id, cols, rows, clock())
            }

            is ClientEvent.Sessions -> {
                // `Sessions` pages are ended by a `last` flag and carry no `first`: a new answer
                // starts when the previous one finished, which is exactly when `listing` is null.
                val open = listing ?: mutableListOf<SessionEntry>().also { listing = it }
                open.addAll(event.message.entries)
                if (event.message.last) {
                    val entries = listing ?: return
                    listing = null
                    publishList(entries)
                }
            }

            is ClientEvent.Marks -> {
                // Marks ride the list answer, so a `Marks` and a `Sessions` arrive together. A
                // message that starts a set replaces what was held; one that continues it adds,
                // and a kind this build does not know is the host clearing the mark.
                val next = if (event.message.first) mutableMapOf() else _marks.value.toMutableMap()
                for (m in event.message.entries) {
                    val mark = ShellMark.of(m.kind)
                    if (mark == null) next.remove(m.id) else next[m.id] = mark
                }
                _marks.value = next
            }

            is ClientEvent.History -> {
                lastHeardAt = clock()
                onHistoryPage?.invoke(event.message)
            }
        }
    }

    /**
     * A finished list, as the screen shows it: one shell becomes the page, none is a sentence, and
     * several is a choice. **Nothing is guessed** — a link that picked the first entry would open a
     * shell the operator did not name.
     */
    private suspend fun publishList(entries: List<SessionEntry>) {
        _shells.value = MeshShells(entries, _shells.value.current)
        when {
            entries.isEmpty() -> _view.value = MeshTermView.NoShells
            entries.size == 1 && listing == null && page.isEmpty() -> {
                // A single shell and no preference: opening it is not a choice, it is the answer.
                page = entries.first().id
                attaching = true
                _view.value = MeshTermView.ProfileMissing
                val (cols, rows) = grid ?: (0 to 0)
                engine.open(page, cols, rows, clock())
            }
            page.isEmpty() -> _view.value = MeshTermView.Choose(entries)
            else -> Unit
        }
    }

    /**
     * The engine's mirror as the screen's screen.
     *
     * The rows are read one at a time — `ldmux_client` copies into a caller buffer and hands back
     * nothing it owns — and a row the engine does not have ends the screen, because a shorter screen
     * is a fact and a padded one is an invention.
     */
    private suspend fun screenOf(shell: String, mirror: ClientMirror): MeshScreen? {
        if (!mirror.painted || mirror.cols <= 0 || mirror.rows <= 0) return null
        val lines = ArrayList<String>(mirror.rows)
        for (row in 0 until mirror.rows) {
            val bytes = engine.mirrorLine(shell, row) ?: break
            lines.add(decodeUtf8(bytes))
        }
        if (lines.isEmpty()) return null
        return MeshScreen(
            lines = lines,
            cursorX = mirror.cursorX,
            cursorY = mirror.cursorY,
            alternate = mirror.alternate,
            dead = mirror.dead,
            epoch = mirror.snapshotId,
            cols = mirror.cols,
            firstLine = mirror.firstLine,
        )
    }

    /**
     * UTF-8 to a Kotlin string, without replacing a character the host deliberately sent.
     *
     * `decodeToString` is the platform's own decoder and is used rather than a hand-rolled loop: a
     * second UTF-8 reader in the app is a second thing to be wrong about, and the rows are already
     * validated where they are cut.
     */
    private fun decodeUtf8(bytes: ByteArray): String = bytes.decodeToString()

    // ── calls the screen makes, each one a fact the engine already owns ──────────────────────

    /** The grid the surface can paint at, remembered for the next page and sent on the next fetch. */
    suspend fun setGrid(cols: Int, rows: Int) {
        if (cols <= 0 || rows <= 0) return
        grid = cols to rows
    }

    /** One command: the whole draft as a single message. */
    suspend fun input(bytes: ByteArray): InputOutcome = send(bytes, ClientCode.ITEM_TEXT)

    /**
     * The draft as **one** command.
     *
     * The items go to the engine as an item stream, which the library decodes with the same function
     * the wire uses and folds into a single `T_SEND`. Sending the text and its `Enter` as two calls
     * would be two messages — `RC-8` — which is the whole reason this member exists rather than the
     * screen calling [input] and [keys] in turn.
     */
    suspend fun sendDraft(items: List<DraftItem>): Boolean {
        if (items.isEmpty()) return false
        return send(ItemStream.encode(items), ClientCode.ITEM_STREAM) == InputOutcome.Accepted
    }

    /** Key names, single-space separated, encoded by the host for the pane's live modes. */
    suspend fun keys(names: List<String>): InputOutcome =
        send(names.joinToString(" ").encodeToByteArray(), ClientCode.ITEM_KEYS)

    private suspend fun send(payload: ByteArray, kind: Int): InputOutcome {
        if (payload.isEmpty()) return InputOutcome.Empty
        val shell = page
        if (shell.isEmpty()) return InputOutcome.NotLive
        val (cols, rows) = grid ?: (0 to 0)
        return when (engine.send(shell, payload, kind, cols, rows, clock())) {
            ClientCode.OK -> InputOutcome.Accepted
            ClientCode.ERR_TOO_LONG -> InputOutcome.TooLong
            ClientCode.ERR_QUEUE_FULL -> InputOutcome.QueueFull
            ClientCode.ERR_NOT_OPEN, ClientCode.ERR_NO_PAGE -> InputOutcome.NotLive
            else -> InputOutcome.NotLive
        }
    }

    /** Open one shell's page, or ask for the list when [id] is empty. */
    suspend fun open(id: String): Boolean {
        if (id.isEmpty()) return launcher()
        page = id
        attaching = true
        _view.value = MeshTermView.ProfileMissing
        val (cols, rows) = grid ?: (0 to 0)
        val rc = engine.open(id, cols, rows, clock())
        if (rc == ClientCode.OK) {
            _shells.value = _shells.value.copy(current = id)
            return true
        }
        attaching = false
        if (rc == ClientCode.ERR_ALREADY_OPEN) return true
        return false
    }

    suspend fun selectShell(id: String): Boolean = open(id)

    /** Back to the list: end the page, then ask what is running. */
    suspend fun launcher(): Boolean {
        if (page.isNotEmpty()) {
            engine.closePage(page)
            page = ""
        }
        _view.value = MeshTermView.Listing
        engine.list(clock())
        return true
    }

    suspend fun listShells() {
        _view.value = MeshTermView.Listing
        engine.list(clock())
    }

    suspend fun refresh() = listShells()

    suspend fun createShell(cwd: String): Boolean =
        engine.createShell(cwd, clock()) == ClientCode.OK

    /** End a shell, or let go of it when a desk window holds it. */
    suspend fun closeShell(id: String): Boolean {
        val rc = engine.end(id, clock())
        if (id == page) {
            page = ""
            attaching = false
            lastScreen = null
            lastSnapshot = 0
        }
        return rc == ClientCode.OK
    }

    /** Open the shell in a desk window on the host. */
    suspend fun makeLive(id: String): Boolean = engine.makeLive(id, clock()) == ClientCode.OK

    /** Take a shell another device holds. */
    suspend fun takeover(id: String): Boolean {
        val rc = engine.takeover(id, clock())
        if (rc != ClientCode.OK) return false
        page = id
        attaching = true
        _view.value = MeshTermView.ProfileMissing
        return true
    }

    /** A page of the shell's own scrollback, [rows] rows from absolute line [from]. */
    suspend fun wantHistory(from: Int, rows: Int) {
        if (page.isEmpty()) return
        engine.history(page, from, rows, clock())
    }

    /** Ask for the screen again. Never touches the draft. */
    suspend fun resync() {
        if (page.isEmpty()) return
        val (cols, rows) = grid ?: (0 to 0)
        engine.fetch(page, cols, rows, clock())
    }

    /** The batch model's `show`: in v15 it is exactly a refresh. */
    suspend fun showOnce() = resync()

    /** The page's screen as the driver last applied it, for a caller that needs it synchronously. */
    fun lastPainted(): MeshScreen? = lastScreen

    /** The newest snapshot id this driver applied. */
    fun snapshotId(): Int = lastSnapshot

    /**
     * The engine's own reading of the link, in the screen's vocabulary.
     *
     * **The one place that decides which `MeshTermView` a link fact means.** The engine publishes
     * four facts and never a view; the screen draws a view and never reads a fact. Called by the
     * screen's ticker, which is also how the handshake's ladder becomes a verdict rather than a
     * spinner.
     */
    suspend fun syncLinkState() {
        if (closed) return
        val state = engine.linkState.value
        val detail = engine.problem.value
        when (state) {
            EngineLink.RadioMissing ->
                if (_view.value !is MeshTermView.Live) {
                    _view.value = MeshTermView.LinkDown(MeshLinkState.NeedsRadio, detail)
                }

            EngineLink.PermissionDenied ->
                _view.value = MeshTermView.LinkDown(MeshLinkState.NeedsPermission, detail)

            EngineLink.LinkDown ->
                if (_view.value !is MeshTermView.Live) {
                    _view.value = MeshTermView.LinkDown(MeshLinkState.RadioUnavailable, detail)
                }

            EngineLink.Connected -> {
                // Established and nothing painted yet is a handshake in progress; the number of
                // rungs spent is what turns that spinner into a verdict.
                if (!engine.established()) {
                    val rungs = engine.stats()?.rungsUsed ?: 0
                    _attempt.value = rungs.toInt()
                    _view.value = if (rungs >= NO_ANSWER_RUNGS) {
                        MeshTermView.NoAnswer
                    } else {
                        MeshTermView.Handshaking
                    }
                } else if (attaching) {
                    _view.value = MeshTermView.ProfileMissing
                }
            }
        }
        lastStats = engine.stats()
    }

    /** Close the link, the lease and the engine's loop. Idempotent. */
    fun close() {
        if (closed) return
        closed = true
        engine.close()
    }

    /** The engine's counters as of the last event, for a log line. */
    fun stats(): ClientStats? = lastStats

    private companion object {
        /** A record budget larger than any hour's, for a band that has no allowance. */
        const val UNMETERED = 255

        /**
         * Rungs spent with no session before "trying" becomes "no answer".
         *
         * Three is the ladder's own length: a fourth ask is not a longer attempt, it is a link that
         * is not coming back, and the screen owes the operator that sentence rather than a counter.
         */
        const val NO_ANSWER_RUNGS = 3L
    }
}
