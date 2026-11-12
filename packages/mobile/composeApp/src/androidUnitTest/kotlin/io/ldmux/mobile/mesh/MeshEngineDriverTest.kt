package io.ldmux.mobile.mesh

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.runBlocking
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * The translator, on its own.
 *
 * **Not a protocol test.** Everything asserted here is a mapping: an engine fact in, the screen's
 * vocabulary out. The bytes, the retries and the deadlines are the library's and are tested where
 * they live (`crates/session/tests/`); a second set of assertions about them here would be a second
 * opinion about the wire, which is what `docs/RADIO-PLAN.md` §7 removed.
 */
class MeshEngineDriverTest {

    /** The engine, as far as the translator can tell: facts in, calls recorded. */
    private class FakeEngine : RadioEngine {
        val up = MutableStateFlow(false)
        val problemState = MutableStateFlow<String?>(null)
        val link = MutableStateFlow(EngineLink.LinkDown)
        val radioState = MutableStateFlow(RadioStatus.UNKNOWN)
        val routeState = MutableStateFlow(Route.UNKNOWN)
        private val eventFlow = MutableSharedFlow<ClientEvent>(extraBufferCapacity = 32)

        override val linkUp: StateFlow<Boolean> = up.asStateFlow()
        override val linkState: StateFlow<EngineLink> = link.asStateFlow()
        override val problem: StateFlow<String?> = problemState.asStateFlow()
        override val radio: StateFlow<RadioStatus> = radioState.asStateFlow()
        override val route: StateFlow<Route> = routeState.asStateFlow()
        override val events: Flow<ClientEvent> = eventFlow.asSharedFlow()

        var opened_ = true
        override val isOpen: Boolean get() = opened_

        var establishedNow = false
        var rungs = 0L
        val sent = mutableListOf<Triple<String, ByteArray, Int>>()
        val opened = mutableListOf<String>()
        val fetched = mutableListOf<String>()
        val histories = mutableListOf<Triple<String, Int, Int>>()
        val closedPages = mutableListOf<String>()
        var listed = 0
        var created = mutableListOf<String>()
        var marksAsked = 0

        /** The rows the mirror will report, per shell. */
        val rows = mutableMapOf<String, List<String>>()
        val mirrors = mutableMapOf<String, ClientMirror>()

        override suspend fun start() {}
        override suspend fun connect(nowMs: Long, wallMs: Long): Int = ClientCode.OK
        override suspend fun established(): Boolean = establishedNow
        override suspend fun setPreset(
            freqKhz: Int, bwHz: Int, sf: Int, cr: Int, pathWidth: Int, powerDbm: Int,
        ): Int = ClientCode.OK

        override suspend fun setScope(key: ByteArray): Int = ClientCode.OK
        override suspend fun setRecordsLeft(left: Int) {}

        override suspend fun open(shell: String, cols: Int, rows: Int, nowMs: Long): Int {
            opened.add(shell)
            return ClientCode.OK
        }

        override suspend fun closePage(shell: String): Int {
            closedPages.add(shell)
            return ClientCode.OK
        }

        override suspend fun end(shell: String, nowMs: Long): Int = ClientCode.OK
        override suspend fun createShell(cwd: String, nowMs: Long): Int {
            created.add(cwd)
            return ClientCode.OK
        }

        override suspend fun takeover(shell: String, nowMs: Long): Int = ClientCode.OK
        override suspend fun makeLive(shell: String, nowMs: Long): Int = ClientCode.OK

        override suspend fun history(shell: String, from: Int, rows: Int, nowMs: Long): Int {
            histories.add(Triple(shell, from, rows))
            return ClientCode.OK
        }

        override suspend fun list(nowMs: Long): Int {
            listed += 1
            return ClientCode.OK
        }

        override suspend fun send(
            shell: String, text: ByteArray, kind: Int, cols: Int, rows: Int, nowMs: Long,
        ): Int {
            sent.add(Triple(shell, text, kind))
            return ClientCode.OK
        }

        override suspend fun fetch(shell: String, cols: Int, rows: Int, nowMs: Long): Int {
            fetched.add(shell)
            return ClientCode.OK
        }

        override suspend fun mirror(shell: String): ClientMirror? = mirrors[shell]

        override suspend fun mirrorLine(shell: String, row: Int): ByteArray? =
            rows[shell]?.getOrNull(row)?.encodeToByteArray()

        override suspend fun stats(): ClientStats? = ClientStats(
            recordsSent = 0, recordsReceived = 0, commandsSent = sent.size.toLong(),
            fetchesSent = fetched.size.toLong(), snapshotsApplied = 0, snapshotsRefused = 0,
            sessionsEstablished = if (establishedNow) 1 else 0, rungsUsed = rungs,
            blindAsks = 0, routeAnswered = false, radioRecords = 0, radioAnswered = 0,
            radioExpired = 0,
        )

        override suspend fun nextDeadline(): Long? = null
        override fun onNotification(frame: ByteArray) {}
        override fun close() {
            opened_ = false
        }

        fun mirrorOf(shell: String, lines: List<String>, cursorY: Int = 0) {
            rows[shell] = lines
            mirrors[shell] = ClientMirror(
                painted = true, cols = 46, rows = lines.size, cursorX = 2, cursorY = cursorY,
                alternate = false, dead = false, settled = true, hasApplied = true,
                appliedStatus = 1, snapshotId = 7, firstLine = 0,
            )
        }
    }

    private fun driver(engine: FakeEngine, selector: String = "shell-1") = MeshEngineDriver(
        engine = engine,
        clock = { 1_000 },
        wall = { 1_700_000_000_000 },
        selector = selector,
    )

    /** A page's first screen makes it `Live`, with the rows the mirror reported. */
    @Test
    fun a_painted_screen_becomes_the_live_view() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine)
        engine.mirrorOf("shell-1", listOf("one", "two"))
        // Opening a page asks the engine for it and waits for the first screen.
        d.open("shell-1")
        assertEquals(MeshTermView.ProfileMissing, d.view.value, "opening waits for the screen")

        d.apply(ClientEvent.Settled("shell-1", 7, true, true, 1))
        val live = d.view.value
        assertTrue(live is MeshTermView.Live, "a settled screen is Live, got $live")
        assertEquals(listOf("one", "two"), (live as MeshTermView.Live).screen.lines)
        assertEquals(46, live.screen.cols)
        assertEquals(2, live.screen.cursorX)
    }

    /** A held shell is `HeldBy`, not a silent steal and not a blank page. */
    @Test
    fun a_held_shell_is_named() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine)
        d.apply(ClientEvent.Held("shell-1"))
        assertEquals(MeshTermView.HeldBy("shell-1"), d.view.value)
    }

    /** The host ending the page carries its reason, so the screen can say which ending it was. */
    @Test
    fun the_host_closing_carries_its_reason() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine)
        val closed = TermMessage.Closed(MeshTerm.Closed.HELD)
        d.apply(ClientEvent.Closed("shell-1", MeshTerm.Closed.HELD, closed))
        assertEquals(MeshTermView.HostClosed(MeshTerm.Closed.HELD), d.view.value)
    }

    /** A radio the user must fix is `NeedsRadio`, and a permission is `NeedsPermission`. */
    @Test
    fun the_four_link_facts_map_to_three_sentences_and_one_silence() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine)

        engine.link.value = EngineLink.RadioMissing
        engine.problemState.value = "no radio is chosen"
        d.syncLinkState()
        assertEquals(
            MeshTermView.LinkDown(MeshLinkState.NeedsRadio, "no radio is chosen"),
            d.view.value,
        )

        engine.link.value = EngineLink.PermissionDenied
        engine.problemState.value = "this app may not use Bluetooth"
        d.syncLinkState()
        assertEquals(
            MeshTermView.LinkDown(MeshLinkState.NeedsPermission, "this app may not use Bluetooth"),
            d.view.value,
        )

        engine.link.value = EngineLink.LinkDown
        engine.problemState.value = "the radio went away"
        d.syncLinkState()
        assertEquals(
            MeshTermView.LinkDown(MeshLinkState.RadioUnavailable, "the radio went away"),
            d.view.value,
        )
    }

    /** A connected link with no session is a handshake, and a spent ladder is a verdict. */
    @Test
    fun a_spent_ladder_is_no_answer_rather_than_a_spinner() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine)
        engine.link.value = EngineLink.Connected

        engine.rungs = 1
        d.syncLinkState()
        assertEquals(MeshTermView.Handshaking, d.view.value)

        engine.rungs = 4
        d.syncLinkState()
        assertEquals(MeshTermView.NoAnswer, d.view.value)
        assertEquals(4, d.attempt.value, "the counter is the ladder the operator is watching")
    }

    /** Several shells and no preference is a choice; none is a sentence; one opens itself. */
    @Test
    fun an_answer_with_several_shells_asks_which_and_with_one_opens_it() = runBlocking {
        val two = FakeEngine()
        val d2 = driver(two, selector = "")
        d2.apply(
            ClientEvent.Sessions(
                "", last = true,
                message = TermMessage.Sessions(
                    last = true,
                    entries = listOf(entry("a"), entry("b")),
                ),
            ),
        )
        val choose = d2.view.value
        assertTrue(choose is MeshTermView.Choose, "two shells is a choice, got $choose")
        assertEquals(listOf("a", "b"), (choose as MeshTermView.Choose).entries.map { it.id })

        val none = FakeEngine()
        val d0 = driver(none, selector = "")
        d0.apply(
            ClientEvent.Sessions("", last = true, message = TermMessage.Sessions(last = true, entries = emptyList())),
        )
        assertEquals(MeshTermView.NoShells, d0.view.value)

        val one = FakeEngine()
        val d1 = driver(one, selector = "")
        d1.apply(
            ClientEvent.Sessions("", last = true, message = TermMessage.Sessions(last = true, entries = listOf(entry("solo")))),
        )
        assertEquals(listOf("solo"), one.opened, "one shell and no preference is the answer, not a choice")
    }

    /** A command before a page is refused with the outcome the screen draws, not sent anywhere. */
    @Test
    fun input_needs_a_page_and_becomes_one_send_when_it_has_one() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine, selector = "")
        assertEquals(InputOutcome.NotLive, d.input("ls\n".encodeToByteArray()))
        assertTrue(engine.sent.isEmpty(), "nothing was sent for a shell that is not open")

        d.open("shell-9")
        assertEquals(InputOutcome.Accepted, d.input("ls -la".encodeToByteArray()))
        assertEquals(1, engine.sent.size, "one command is one message")
        assertEquals("shell-9", engine.sent[0].first)
        assertEquals(ClientCode.ITEM_TEXT, engine.sent[0].third)
        assertEquals("ls -la", engine.sent[0].second.decodeToString())
    }

    /** Key names travel as names, joined, so the host reads the pane's live cursor-key mode. */
    @Test
    fun keys_travel_as_names_in_one_message() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine)
        d.open("shell-1")
        assertEquals(InputOutcome.Accepted, d.keys(listOf("Up", "S-Tab")))
        assertEquals(1, engine.sent.size)
        assertEquals(ClientCode.ITEM_KEYS, engine.sent[0].third)
        assertEquals("Up S-Tab", engine.sent[0].second.decodeToString())
    }

    /** A created shell is opened: the answer to a create must not be swallowed by the list. */
    @Test
    fun a_created_shell_is_opened_and_ends_the_choose_view() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine, selector = "")
        d.apply(ClientEvent.Created("made-1"))
        assertEquals(listOf("made-1"), engine.opened)
        assertEquals(MeshTermView.ProfileMissing, d.view.value)
    }

    /** Marks arrive with the list and are kept per shell. */
    @Test
    fun marks_arrive_with_the_list() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine)
        d.apply(
            ClientEvent.Marks(
                "", last = true,
                message = TermMessage.Marks(
                    first = true, last = true,
                    entries = listOf(MarkEntry("shell-1", ShellMark.Done.wire)),
                ),
            ),
        )
        assertEquals(ShellMark.Done, d.marks.value["shell-1"])
    }

    /** The scroll pad's up key asks for the page above the oldest row this client holds. */
    @Test
    fun history_names_the_page_it_wants() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine)
        d.open("shell-1")
        d.wantHistory(from = 40, rows = 17)
        assertEquals(listOf(Triple("shell-1", 40, 17)), engine.histories)
    }

    /** A refresh asks for the screen and never spends the draft — the brief's central rule. */
    @Test
    fun a_refresh_asks_for_the_screen_and_sends_nothing() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine)
        d.open("shell-1")
        d.resync()
        assertEquals(listOf("shell-1"), engine.fetched)
        assertTrue(engine.sent.isEmpty(), "a refresh must never run what is typed")
    }

    /** Going back to the list ends the page, so the host stops drawing it. */
    @Test
    fun the_launcher_ends_the_page_before_asking_for_the_list() = runBlocking {
        val engine = FakeEngine()
        val d = driver(engine)
        d.open("shell-1")
        d.launcher()
        assertEquals(listOf("shell-1"), engine.closedPages)
        assertTrue(engine.listed >= 1)
    }

    /** The engine's own events are the only clock: the driver reports what they said. */
    @Test
    fun the_answered_age_comes_from_the_last_event() = runBlocking {
        val engine = FakeEngine()
        var now = 1_000L
        val d = MeshEngineDriver(engine, clock = { now }, wall = { 0L }, selector = "shell-1")
        d.apply(ClientEvent.Held("shell-1"))
        now = 4_500L
        assertEquals(3_500L, d.answeredAgoMs())
    }

    private fun entry(id: String) = SessionEntry(id = id, label = id)
}
