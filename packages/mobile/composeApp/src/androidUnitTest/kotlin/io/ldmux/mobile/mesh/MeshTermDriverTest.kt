package io.ldmux.mobile.mesh

import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.io.File
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The driver against fakes: no socket, no native library. Every rule it owns has a case. */
class MeshTermDriverTest {
    private class FakeGateway : GatewayPort {
        override val state = MutableStateFlow(MeshLinkState.Bound)
        override val status = MutableStateFlow<MeshStatus?>(MeshStatus(0, null, 0, 58))
        override val received = MutableSharedFlow<ByteArray>(extraBufferCapacity = 8)
        override var uplinkRecordMax = 172
        /**
         * A neighbour by default, so the ladder tests exercise the ladder.
         *
         * `null` is the blind case — no route to the host — and the driver spends its rungs asking
         * the mesh for one instead of re-sending a direct packet no repeater will carry. Zero is a
         * neighbour, which is the timing every ladder test was written against.
         */
        override var routeHops: Int? = 0
        /** A route in hand that nothing has confirmed lately is as good as none, for the ladder. */
        override var routeFresh: Boolean = true
        val sent = ArrayList<ByteArray>()
        var routeRefreshes = 0
        var outcome = MeshSendOutcome.SentDirect
        /** A gateway that takes the record and never answers. */
        var silent = false
        override suspend fun send(record: ByteArray): MeshSendResult {
            sent += record
            if (silent) kotlinx.coroutines.awaitCancellation()
            return MeshSendResult(0, outcome)
        }
        override suspend fun refreshRoute() {
            routeRefreshes++
        }
    }

    /** What the driver queued, and what it was told to deliver. */
    private class FakeSession : SessionPort {
        val queued = ArrayList<Pair<Int, ByteArray>>()
        val pollCalls = ArrayList<Pair<Int, Int>>()
        var nextReceive: Received = Received.Quiet
        var queueOutcome = QueueOutcome.Queued
        var health = SessionHealth(false, false, false, 0, 0)
        var closedRequested = false
        var freed = false
        private var out = 0
        override fun send(shell: Int, bytes: ByteArray): QueueOutcome {
            if (queueOutcome == QueueOutcome.Queued) queued += shell to bytes
            return queueOutcome
        }
        override fun receive(nowMs: Long, record: ByteArray) = nextReceive
        /** Thrown from [poll] when set, the way a session whose keys have gone says so. */
        var failPoll: MeshSessionException? = null
        override fun poll(nowMs: Long, recordsLeft: Int, l2Slots: Int): List<ByteArray> {
            failPoll?.let { throw it }
            pollCalls += recordsLeft to l2Slots
            // A fake that honours the contract: never more than the budget or the window.
            val n = minOf(recordsLeft, l2Slots, queued.size - out)
            return List(maxOf(n, 0)) { byteArrayOf(out++.toByte()) }
        }
        override fun health(nowMs: Long) = health
        override fun nextDeadlineMs(): Long? = null
        override fun requestClose() { closedRequested = true }
        override fun close() { freed = true }
        fun messages(): List<TermMessage> =
            queued.flatMap { (_, b) -> TermDecoder().push(b).map { (it as TermItem.Message).message } }
    }

    private class FakeHandshake(val session: FakeSession, val fail: MeshSessionException? = null) : HandshakePort {
        override val message1 = ByteArray(121) { 1 }
        var abandoned = false
        override fun finish(message2: ByteArray, recordLimit: Int, nowMs: Long): SessionPort {
            if (fail != null) throw fail
            return session
        }
        override fun abandon() { abandoned = true }
    }

    private val gw = FakeGateway()
    private val sess = FakeSession()
    private var hs = FakeHandshake(sess)
    private var now = 1_000L
    private fun driver(handshakeWaitMs: Long = 300_000, sendTimeoutMs: Long = 10_000) =
        MeshTermDriver(gw, { hs }, selector = "", clock = { now }, handshakeWaitOverride = handshakeWaitMs, sendTimeoutMs = sendTimeoutMs)

    /**
     * **A handshake nobody answers becomes a sentence, not a climbing counter.**
     *
     * The ladder keeps running behind it (that is what heals the link when the host comes back), but
     * after `handshakeVerdictMs` the view says no answer came, because the operator needs to know the
     * host is not there rather than watch seconds pass (measured on the device, 2026-11-07: the host
     * radio answered pushes and not commands, and the phone sat on `handshaking · 59 s`).
     */
    @Test
    fun an_unanswered_handshake_becomes_no_answer_and_the_retrying_goes_on(): Unit = runBlocking {
        val d = driver()
        d.start()
        assertIs<MeshTermView.Handshaking>(d.view.value)
        now += 29_000
        d.tick()
        assertIs<MeshTermView.Handshaking>(d.view.value, "under the verdict it is still securing the link")
        now += 2_000
        d.tick()
        assertIs<MeshTermView.NoAnswer>(d.view.value, "past thirty seconds the app says nothing came back")
        // ... and the ladder is untouched: it asks again, which is what heals it when the host returns.
        val asked = gw.sent.size
        now += 40_000
        d.tick()
        assertTrue(gw.sent.size > asked, "the retrying continues behind the verdict")
    }

    /**
     * **A shell being opened asks where the host is.**
     *
     * The session survives a walk on retransmissions, so a link that is up can be holding a route
     * that no longer exists; the maintenance loop would notice within a minute, and the first record
     * of this shell is what is being sent now. The ask happens before that record, and once.
     */
    @Test
    fun opening_a_shell_re_reads_the_route_first(): Unit = runBlocking {
        val d = driver()
        d.start()
        assertEquals(1, gw.routeRefreshes, "one re-read per open, before the first record")
        assertIs<MeshTermView.Handshaking>(d.view.value, "the open carried on past the re-read")
    }

    /** An unanswered handshake is asked again in seconds, not waited out. */
    @Test
    fun an_unanswered_handshake_is_asked_again_with_new_keys(): Unit = runBlocking {
        val d = driver()
        d.start()
        assertIs<MeshTermView.Handshaking>(d.view.value)
        val first = gw.sent.size
        now += 5_000
        d.tick()
        assertEquals(first, gw.sent.size, "five seconds is not yet a retry")
        now += 3_500
        d.tick()
        assertEquals(first + 1, gw.sent.size, "over eight seconds it asks again")
        // **The second ask waits twice as long.** A fixed short interval re-keys before the answer
        // can arrive, and then hands the late `msg2` to a handshake it no longer belongs to
        // (measured 2026-11-01: five accepted handshakes on the host, the phone still saying
        // `handshaking`). Doubling from 8 s is CoAP's ladder.
        now += 9_000
        d.tick()
        assertEquals(first + 1, gw.sent.size, "the second wait is the doubled one")
        now += 7_500
        d.tick()
        assertEquals(first + 2, gw.sent.size, "and the next ask comes at sixteen")
        assertIs<MeshTermView.Handshaking>(d.view.value, "and the screen still says so")
    }

    /// **The handshake's own ladder is what the loop wakes for.**
    ///
    /// The screen's loop sleeps until `nextWakeMs` says something is due, and that list did not
    /// include the handshake: with no session there is no session deadline, so the loop fell back to
    /// the 30 s idle tick and looked at the ladder only once per tick. Every figure in the ladder —
    /// 8 s, then 16, then 32, and the total budget built to outlast them — was therefore fiction on
    /// the device. Measured 2026-11-10: the operator reported the handshake timing out "after thirty
    /// seconds", which is the idle tick, not the ladder.
    @Test
    fun the_loop_wakes_for_the_handshakes_own_rungs(): Unit = runBlocking {
        val d = driver()
        val began = now
        d.start()
        assertIs<MeshTermView.Handshaking>(d.view.value)
        // The first ask has just gone: the next rung is eight seconds away, and the loop must know it
        // rather than discover it on the next idle tick.
        assertEquals(now + handshakeRetryFor(null), d.nextWakeMs(), "the first re-ask is scheduled")
        now += handshakeRetryFor(null)
        d.tick()
        // The second rung is the doubled one, measured from the re-ask that just happened.
        assertEquals(now + (handshakeRetryFor(null) shl 1), d.nextWakeMs(), "and then the doubled one")
        // The whole handshake's budget is a deadline of its own: whichever comes first, the loop must
        // never sleep past the moment the handshake is given up on.
        val budget = began + handshakeWaitMs(null)
        assertTrue(
            (d.nextWakeMs() ?: Long.MAX_VALUE) <= budget,
            "the loop must not sleep past the handshake's budget",
        )
    }

    /// **With no route, the rungs ask the mesh instead of re-sending a handshake.**
    ///
    /// A session record is a direct packet and a direct packet crosses no repeater, so a handshake
    /// with no route can only reach the operator's immediate neighbours — at range it is air spent on
    /// something that cannot arrive. A route request is a flood and does cross. Measured intent,
    /// 2026-11-10: the phone climbed the whole ladder at range and the host logged no handshake at
    /// all, because none of them could get there.
    @Test
    fun with_no_route_the_rungs_ask_the_mesh_instead_of_re_asking_a_handshake(): Unit = runBlocking {
        val d = driver()
        gw.routeHops = null
        d.start()
        assertIs<MeshTermView.Handshaking>(d.view.value)
        val records = gw.sent.size
        val asks = gw.routeRefreshes
        now += handshakeRetryFor(null)
        d.tick()
        assertEquals(records, gw.sent.size, "no handshake was re-sent down a path that does not exist")
        assertTrue(gw.routeRefreshes > asks, "and a route request went out instead")
        assertIs<MeshTermView.Handshaking>(d.view.value, "the screen still says it is working on it")
        // The verdict names the real thing when the budget runs out. The view does not rest on it —
        // a failure schedules its own recovery, which is deliberate — so this asserts the sentence
        // the app logs, which is the same one the failure dump carries off the phone.
        // Driven rung by rung, because the budget is measured from the moment the handshake began
        // and the test's clock is not that moment.
        var rungs = 0
        while (io.ldmux.mobile.AppLogBuffer.snapshot().none { it.contains("no route") } &&
            rungs++ < 40
        ) {
            now += handshakeRetryFor(null)
            d.tick()
        }
        // **But not for ever.** An asymmetric link can hear a direct packet while losing the flood,
        // so a blind handshake is still tried on every third rung rather than abandoned.
        assertTrue(
            gw.sent.size > records,
            "a blind handshake is still attempted, just not on every rung",
        )
        assertTrue(
            io.ldmux.mobile.AppLogBuffer.snapshot().any { it.contains("no route") },
            "the verdict says no route, not 'three unanswered handshakes': " +
                io.ldmux.mobile.AppLogBuffer.snapshot().takeLast(3).toString(),
        )
    }

    /// **A route nothing has confirmed lately is treated as no route by the ladder.**
    ///
    /// Measured 2026-11-10: three minutes of 8/16/32 s rungs handshaking down a stale 0-hop route,
    /// the host hearing none of them, while the phone asked for a route only every half minute. A
    /// stale route is still non-null, so the "ask the mesh instead" rule never engaged.
    @Test
    fun a_stale_route_makes_the_rungs_ask_the_mesh_too(): Unit = runBlocking {
        val d = driver()
        gw.routeHops = 3
        gw.routeFresh = false
        d.start()
        val records = gw.sent.size
        val asks = gw.routeRefreshes
        now += handshakeRetryFor(3)
        d.tick()
        assertEquals(records, gw.sent.size, "no handshake down a route nothing has confirmed")
        assertTrue(gw.routeRefreshes > asks, "a route request went out, and a request is a flood")
    }

    /// **A list answer that never arrives is asked for again, and only a few times.**
    ///
    /// `launcher()` marks the request in flight and `listShells()` refuses to ask twice ("a list
    /// already in flight is not asked for again"), so a lost answer left the screen on "asking host
    /// for shells" until the operator pressed refresh — measured 2026-11-09 at 75 s on a link that
    /// was up. The retry is one record a deadline, bounded: a host that is not answering is a state
    /// the screen already draws, and the operator's refresh is the cure.
    @Test
    fun `a list with no answer is asked for again and then left to the operator`() = runBlocking<Unit> {
        val d = driver(); live(d)
        assertTrue(d.launcher(), "the launcher asks for the list")
        assertEquals(now + 8_000, d.nextWakeMs(), "the loop must wake for the deadline")
        val asked = gw.sent.size

        // Nothing arrives: one record a deadline, not one a tick.
        now += 5_000
        d.tick()
        assertEquals(asked, gw.sent.size, "under the deadline nothing is repeated")
        now += 5_000
        d.tick()
        assertEquals(asked + 1, gw.sent.size, "past it the list is asked for once more")

        // The answer arrives: the deadline is over and nothing more is spent on it.
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11"))))
        val answered = gw.sent.size
        now += 40_000
        d.tick()
        assertEquals(answered, gw.sent.size, "an answered list is not asked for again")

        // A host that never answers is asked a bounded number of times, and the count ends.
        assertTrue(d.launcher(), "the list is asked for again from the launcher")
        val start = gw.sent.size
        repeat(8) {
            now += 9_000
            d.tick()
        }
        val repeatAsks = gw.sent.size - start
        assertTrue(repeatAsks in 1..3, "two more asks at most, not one per deadline: $repeatAsks")
    }

    /// **A `show` that draws nothing does not start the resync ladder.**
    ///
    /// `resyncing` is the missing-screen flag: while it is set the tick re-asks on a doubling
    /// deadline, up to once a minute. A `show` on an idle shell legitimately draws nothing (nothing
    /// changed), and leaving the flag set would spend a record a minute for ever on a frozen view.
    @Test
    fun `a show does not start the resync ladder`() = runBlocking<Unit> {
        val d = driver(); live(d)
        val before = gw.sent.size
        d.showOnce()
        now += 30_000
        d.tick()
        now += 60_000
        d.tick()
        assertEquals(before + 1, gw.sent.size, "one `Resync`, and no ladder after it")
    }

    /// **An attach whose profile never arrives is asked for again, a bounded few times.**
    ///
    /// The tick re-asks for a *screen* when one is missing, but that needs a profile; nothing
    /// re-asked for the profile itself. A host that could not queue it left the page saying
    /// "linked, no terminal" for the life of the connection, with the refresh square as the only
    /// cure — the same shape as the list answer that never came.
    @Test
    fun `an attach with no profile is asked for again`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Sessions(true, listOf(ent("abcd-1"))))
        assertEquals(1, sess.queued.size, "the attach went")
        assertEquals(now + 6_000, d.nextWakeMs(), "the loop must wake for the deadline")

        // Nothing comes back: one record a deadline, then the asks stop.
        now += 3_000
        d.tick()
        assertEquals(1, sess.queued.size, "under the deadline nothing is repeated")
        now += 4_000
        d.tick()
        assertEquals(2, sess.queued.size, "past it the attach is asked for once more")
        repeat(4) {
            now += 7_000
            d.tick()
        }
        assertTrue(sess.queued.size <= 4, "bounded asks, not one per deadline: ${sess.queued.size}")

        // The profile arrives: the deadline is over.
        deliver(d, TermMessage.Profile(13, 46, 17, 0, "abcd-1"))
        assertEquals(null, d.nextWakeMs(), "an answered attach schedules nothing")
    }

    /// **A `Takeover` whose profile never arrived is retried as a `Takeover`.**
    ///
    /// An `Attach` retry would be refused by the desk's claim, and the page would then say the shell
    /// is held by the very window the user had just displaced.
    @Test
    fun `a takeover with no profile is retried as a takeover`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11"))))
        sess.queued.clear()
        assertTrue(d.takeover("aa11"))
        assertEquals(1, sess.messages().count { it is TermMessage.Takeover })

        now += 7_000
        d.tick()
        assertEquals(
            2,
            sess.messages().count { it is TermMessage.Takeover },
            "the same ask, repeated: ${sess.messages()}"
        )
        assertTrue(
            sess.messages().none { it is TermMessage.Attach },
            "and never an Attach, which the claim would refuse: ${sess.messages()}"
        )
    }

    /// **A `Create` whose answer never came is asked for again.**
    ///
    /// The list row that starts a shell says "starting · N s" while it waits; a lost answer left it
    /// there for ever, with nothing spent to say why.
    @Test
    fun `a create with no answer is asked for again`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Profile(13, 46, 17, 0, "aa11"))
        sess.queued.clear()
        assertTrue(d.createShell("~/Documents"))
        assertEquals(1, sess.messages().count { it is TermMessage.Create })

        now += 7_000
        d.tick()
        assertEquals(
            2,
            sess.messages().count { it is TermMessage.Create },
            "the create is asked for once more: ${sess.messages()}"
        )
        assertEquals(
            TermMessage.Create(MeshTerm.VERSION, "~/Documents"),
            sess.messages().last { it is TermMessage.Create }
        )

        // The answer arrives: nothing more is asked for.
        deliver(d, TermMessage.Created("bb22"))
        val after = sess.messages().count { it is TermMessage.Create }
        now += 30_000
        d.tick()
        assertEquals(
            after,
            sess.messages().count { it is TermMessage.Create },
            "an answered create is not repeated"
        )
    }

    private fun stream(vararg m: TermMessage): ByteArray = m.flatMap { it.encode().toList() }.toByteArray()
    private fun data(vararg m: TermMessage) = Received.Data(listOf(Delivered(0, stream(*m))))

    private suspend fun deliver(d: MeshTermDriver, vararg m: TermMessage) {
        sess.nextReceive = data(*m)
        d.onRecord(byteArrayOf(9))
    }

    private val profile = TermMessage.Profile(1, 20, 3, MeshTerm.Unsupported.run {
        SCROLLBACK or STYLE or HYPERLINKS or MOUSE or IMAGES or RESIZE or CLIPBOARD
    })

    private fun rows(epoch: Int, full: Boolean, vararg p: Pair<Int, String>) =
        TermMessage.Rows(epoch, full, p.map { TermPatch(it.first, false, it.second) })

    /** A session the host stopped answering is retried on a schedule, not left dead. */
    @Test
    fun a_dead_session_ends_the_connection_so_it_can_be_rebuilt(): Unit = runBlocking {
        val d = driver()
        live(d)
        sess.health = SessionHealth(failed = true, peerClosed = false, needsRekey = false, pending = 3, dropped = 0)
        d.tick()
        assertIs<MeshTermView.LinkDown>(d.view.value, "the screen says the host stopped answering")
        assertTrue(sess.freed, "the dead session is dropped, not kept")
        assertNull(d.nextWakeMs(), "no retry is scheduled: this socket cannot be revived from here")
        // The run ends so the caller can build a new connection - a new socket, a new handshake.
        // Re-handshaking over a transport that stopped carrying records is what made the phone
        // sit on "securing the link" until the refresh button rebuilt everything by hand.
        val done = withTimeout(5_000) { d.run(); true }
        assertTrue(done, "run returns instead of retrying into a dead transport")
    }

    // The retry backoff (two seconds doubling to thirty) is the screen's, not the driver's: the
    // retry is now "build a new connection", so it lives where the connection is built
    // (`MeshTerminalScreen`, `rebuilds`).

    /** A refused pairing is not retried: waiting cannot cure a token or a key. */
    @Test
    fun a_refused_pairing_is_not_retried(): Unit = runBlocking {
        val d = MeshTermDriver(
            gw,
            { throw MeshSessionException(MeshLinkState.Refused, 1, "bad token") },
            selector = "",
            clock = { now },
        )
        d.start()
        assertIs<MeshTermView.LinkDown>(d.view.value)
        assertNull(d.nextWakeMs(), "nothing is scheduled: the screen offers the cure instead")
        assertTrue(withTimeout(5_000) { d.run(); true }, "and the connection ends")
    }

    private suspend fun live(d: MeshTermDriver) {
        d.start(); d.onRecord(byteArrayOf(2))
        deliver(d, profile, rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))
    }

    @Test
    fun `an unbound gateway is a visible link-down and nothing is sent`() = runBlocking<Unit> {
        gw.state.value = MeshLinkState.RadioUnavailable
        val d = driver(); d.start()
        assertEquals(MeshTermView.LinkDown(MeshLinkState.RadioUnavailable), d.view.value)
        assertTrue(gw.sent.isEmpty())
    }

    @Test
    fun `a gateway that never answers cannot hold the driver, so the tick can still run`() = runBlocking<Unit> {
        gw.silent = true
        val d = driver(sendTimeoutMs = 50)
        d.start() // would never return if the send were awaited without a bound, with the lock held
        assertEquals(MeshTermView.Handshaking, d.view.value)
        d.tick() // and the lock is free again: this is the call that reports a dead link
        assertEquals(MeshTermView.Handshaking, d.view.value)
    }

    @Test
    fun `start sends handshake message one as the first record and waits`() = runBlocking<Unit> {
        val d = driver(); d.start()
        assertEquals(MeshTermView.Handshaking, d.view.value)
        assertContentEquals(hs.message1, gw.sent.single())
    }

    @Test
    fun `message two establishes the session and queues Attach, and the view says no profile yet`() = runBlocking<Unit> {
        val d = driver(); d.start(); d.onRecord(byteArrayOf(2))
        assertEquals(MeshTermView.ProfileMissing, d.view.value)
        assertEquals(listOf<TermMessage>(TermMessage.Attach(MeshTerm.VERSION, "")), sess.messages())
    }

    // ── choosing a shell ───────────────────────────────────────────────────────────────


    /// **The ✕ updates the list the screen draws, not only the shell state.**
    ///
    /// When the view is `Choose` its rows come from `choices`, so dropping the shell from the
    /// driver's own list left the row on screen: closing a phone-opened shell appeared to do
    /// nothing at all until a refresh replaced the view. Reported twice, fixed in the wrong place
    /// twice, and this is the level it lives at.
    @Test
    fun `closing a shell removes its row from the list on screen`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS), TermMessage.Sessions(true, listOf(ent("aa11"), ent("bb22"))))
        assertEquals(MeshTermView.Choose(listOf(ent("aa11"), ent("bb22"))), d.view.value)
        d.closeShell("aa11")
        assertEquals(
            MeshTermView.Choose(listOf(ent("bb22"))),
            d.view.value,
            "the closed shell must be gone from the view the screen draws",
        )
        // And closing the last one is not an empty list, it is "no shells".
        d.closeShell("bb22")
        assertEquals(MeshTermView.NoShells, d.view.value)
    }

    private fun ent(id: String, label: String = "") = SessionEntry(id, label)
    private suspend fun probed(d: MeshTermDriver) { d.start(); d.onRecord(byteArrayOf(2)) }

    /** Reads one mark off the wire as the host would page it. */
    private fun mark(id: String, kind: ShellMark) = MarkEntry(id, kind.wire)

    /**
     * **A page is not the table.** A dozen marked shells do not fit one message, so the host pages
     * them: a page that is not `last` is appended and the page that is `last` replaces the table. A
     * driver that replaced the table per page showed only the last page, which is exactly the case
     * the paging was added for.
     */
    @Test
    fun `mark pages are assembled into one table`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(
            d,
            TermMessage.Marks(false, true, listOf(mark("aa11", ShellMark.Done))),
        )
        assertEquals(mapOf("aa11" to ShellMark.Done), d.marks.value)

        // A run of two pages: the first appends to nothing, the second closes the run.
        deliver(
            d,
            TermMessage.Marks(true, false, listOf(mark("aa11", ShellMark.Done), mark("bb22", ShellMark.Attention))),
            TermMessage.Marks(false, true, listOf(mark("cc33", ShellMark.Done))),
        )
        assertEquals(
            mapOf("aa11" to ShellMark.Done, "bb22" to ShellMark.Attention, "cc33" to ShellMark.Done),
            d.marks.value,
        )

        // A new run replaces the table, and an empty closed page clears it.
        deliver(d, TermMessage.Marks(true, true, emptyList()))
        assertEquals(emptyMap<String, ShellMark>(), d.marks.value)
    }

    /** A half-arrived run must not leak into the next one. */
    @Test
    fun `a lost last page does not merge into the next run`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Marks(true, false, listOf(mark("aa11", ShellMark.Done))))
        // The tail never arrives; the next run starts fresh.
        deliver(d, TermMessage.Marks(true, true, listOf(mark("bb22", ShellMark.Attention))))
        assertEquals(mapOf("bb22" to ShellMark.Attention), d.marks.value)
    }

    @Test
    fun `an ambiguous host is a question, not an ending, and nothing is attached until the user chooses`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS))
        assertEquals(MeshTermView.Listing, d.view.value)
        assertEquals(listOf<TermMessage>(TermMessage.Attach(MeshTerm.VERSION, ""), TermMessage.ListRequest), sess.messages())
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11", "RCNT"), ent("bb22", "pi"))))
        assertEquals(MeshTermView.Choose(listOf(ent("aa11", "RCNT"), ent("bb22", "pi"))), d.view.value)
        assertEquals(2, sess.messages().size, "choosing is the user's: no Attach was sent for them")
    }

    @Test
    fun `choosing sends the chosen id, and an id the host did not list sends nothing`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS), TermMessage.Sessions(true, listOf(ent("aa11"), ent("bb22"))))
        assertEquals(false, d.choose("zz99"))
        assertEquals(2, sess.messages().size)
        assertEquals(true, d.choose("bb22"))
        assertEquals(TermMessage.Attach(MeshTerm.VERSION, "bb22"), sess.messages().last())
        // **Switching shells is the pager's job and it is one `Attach`.** This used to be refused
        // (a second `choose` was not a thing), which is why a page tap over the radio went nowhere
        // and every page showed the first shell's rows.
        assertEquals(true, d.choose("aa11"), "another listed shell is one Attach")
        assertEquals(TermMessage.Attach(MeshTerm.VERSION, "aa11"), sess.messages().last())
        deliver(d, profile, rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))
        assertIs<MeshTermView.Live>(d.view.value)
    }

    @Test
    fun `opening a shell from the launcher works after the shell it was in was released`() = runBlocking<Unit> {
        // The launcher is reached from a live shell, which releases it first (`launcher`). The
        // screen was deciding between two driver methods on the strength of a view state, and the
        // one it picked refuses when nothing is attached, so `open` with three shells ticked did
        // nothing at all.
        val d = driver(); live(d)
        d.listShells()
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11"), ent("bb22"))))
        d.launcher()
        assertEquals(null, d.shells.value.current, "the launcher holds nothing")
        assertTrue(sess.messages().contains(TermMessage.Detach))
        assertTrue(sess.messages().contains(TermMessage.ListRequest))
        val before = sess.messages().size
        assertEquals(true, d.open("bb22"))
        assertEquals(TermMessage.Attach(MeshTerm.VERSION, "bb22"), sess.messages().last())
        assertEquals(before + 1, sess.messages().size)
    }

    @Test
    fun `a list spanning several records is complete only at the last page`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS), TermMessage.Sessions(false, listOf(ent("aa11"))))
        assertEquals(MeshTermView.Listing, d.view.value, "one page is not the answer")
        assertEquals(false, d.choose("aa11"))
        deliver(d, TermMessage.Sessions(true, listOf(ent("bb22"))))
        assertEquals(MeshTermView.Choose(listOf(ent("aa11"), ent("bb22"))), d.view.value)
    }

    /// **A refresh between pages does not truncate the list.**
    ///
    /// The page buffer was emptied whenever a list was asked for, and asking again while the answer
    /// was still arriving threw away every page already in hand: the run's last page then completed
    /// a fresh buffer and the row set became that page alone. Measured on the device 2026-11-09:
    /// twelve shells on the host, two rows on the launcher (the final page), and a refresh showed
    /// all twelve. A request now only says what the next completed run means; the run empties the
    /// buffer.
    /// **A hold is one record per change of state, never one per frame.**
    ///
    /// The screen re-asserts the hold whenever its state changes (mode, waiting, first paint), and
    /// a repeated `Hold` would be a record of airtime for a fact the host already knows.
    /// **A shell ending between two pages does not truncate the list.**
    ///
    /// A `Closed` is the other thing that asks for a list mid-run (`backToList`), and emptying the
    /// buffer there had the same symptom as a refresh: the run's tail became the row set. This is
    /// the failure measured on the device 2026-11-09 (eleven shells, two rows).
    @Test
    fun `a shell ending between pages does not truncate the list`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS), TermMessage.Sessions(false, listOf(ent("aa11"), ent("bb22"))))
        assertEquals(MeshTermView.Listing, d.view.value, "one page is not the answer")
        deliver(d, TermMessage.Closed(MeshTerm.Closed.EXITED))
        deliver(d, TermMessage.Sessions(true, listOf(ent("cc33"))))
        val shown = (d.view.value as? MeshTermView.Choose)?.entries?.map { it.id }
        assertEquals(listOf("aa11", "bb22", "cc33"), shown, "the whole run, not its last page")
    }

    @Test
    fun `a repeated hold is not a second record`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS), TermMessage.Sessions(true, listOf(ent("aa11"), ent("bb22"))))
        assertTrue(d.open("aa11"), "the list's id is opened")

        // **A hold with nothing drawn is not sent.** The host had nothing to draw, so a record
        // saying "stop" would buy nothing.
        d.setHold(true)
        d.flush()
        assertEquals(0, sess.messages().count { it == TermMessage.Hold(true) }, "a silent host is left alone")

        // A frame arrives: now the host is drawing, and the hold is worth its record.
        deliver(d, TermMessage.Profile(12, 46, 17, 0), rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))
        d.flush()
        assertEquals(1, sess.messages().count { it == TermMessage.Hold(true) })

        d.setHold(true)
        d.flush()
        assertEquals(1, sess.messages().count { it == TermMessage.Hold(true) }, "a re-assert is not a record")
        d.setHold(false)
        d.flush()
        assertEquals(1, sess.messages().count { it == TermMessage.Hold(false) })
    }

    /// **A refused hold is retried, and the host is never left believing the wrong state.**
    ///
    /// The old code recorded `holdSent` before the send, so a full queue at that moment suppressed
    /// every retry: the host kept drawing a frozen view at a record a second, or a released shell
    /// stayed unlit until the host's ten-minute expiry.
    @Test
    fun `a hold the session refused is sent again on the next flush`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS), TermMessage.Sessions(true, listOf(ent("aa11"), ent("bb22"))))
        assertTrue(d.open("aa11"))
        deliver(d, TermMessage.Profile(12, 46, 17, 0), rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))

        sess.queueOutcome = QueueOutcome.Full
        d.setHold(true)
        d.flush()
        assertEquals(0, sess.messages().count { it == TermMessage.Hold(true) }, "the queue refused it")

        sess.queueOutcome = QueueOutcome.Queued
        d.setHold(true)
        d.flush()
        assertEquals(1, sess.messages().count { it == TermMessage.Hold(true) }, "and it is sent when there is room")
    }

    /// **A frame from a host that was told to hold means the hold is gone.** Its ten-minute expiry,
    /// or a rebuilt attach, leaves the phone believing a state the host has left; the phone says so
    /// again rather than drawing nothing for ten minutes.
    @Test
    fun `a frame while held re-asserts the hold once`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS), TermMessage.Sessions(true, listOf(ent("aa11"), ent("bb22"))))
        assertTrue(d.open("aa11"))
        deliver(d, TermMessage.Profile(12, 46, 17, 0), rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))
        d.setHold(true)
        d.flush()
        assertEquals(1, sess.messages().count { it == TermMessage.Hold(true) })

        // The host draws anyway: it is no longer holding.
        deliver(d, rows(2, false, 1 to "out"), TermMessage.Commit(2, 2, 1, 0))
        d.flush()
        assertEquals(2, sess.messages().count { it == TermMessage.Hold(true) }, "the hold is re-asserted")
    }

    @Test
    fun `a page run is not truncated by a request arriving between its pages`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS), TermMessage.Sessions(false, listOf(ent("aa11"), ent("bb22"))))
        assertEquals(MeshTermView.Listing, d.view.value, "one page is not the answer")
        // The user comes back to the launcher while the rest of the run is on the air.
        d.launcher()
        deliver(d, TermMessage.Sessions(true, listOf(ent("cc33"))))
        val shown = (d.view.value as? MeshTermView.Choose)?.entries?.map { it.id }
        assertEquals(listOf("aa11", "bb22", "cc33"), shown, "the whole run, not its last page")
    }

    @Test
    fun `no shell is its own state, and asking again that finds exactly one attaches it`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.NOT_FOUND))
        assertEquals(MeshTermView.NoShells, d.view.value)
        d.refresh()
        assertEquals(TermMessage.ListRequest, sess.messages().last())
        assertEquals(MeshTermView.Listing, d.view.value)
        deliver(d, TermMessage.Sessions(true, listOf(ent("cc33"))))
        assertEquals(TermMessage.Attach(MeshTerm.VERSION, "cc33"), sess.messages().last())
    }


    /// **A list the host pushes is applied like one it was asked for.**
    ///
    /// Closing a shell this phone opened makes the host send a fresh `Sessions`, and the driver
    /// threw it away unless the phone had asked for a list: the row stayed until the user pressed
    /// refresh, so the close looked like it had done nothing. The event had arrived and the state
    /// ignored it — the same shape as any "nothing happens until I refresh" defect.
    @Test
    fun `a list the host pushes without being asked replaces the row set`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        // Attached and live on one shell, with the list fetched for the header.
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11", "RCNT"))))
        deliver(d, profile, rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))
        assertIs<MeshTermView.Live>(d.view.value)
        assertEquals(listOf("aa11"), d.shells.value.entries.map { it.id })
        // The host pushes the set that no longer has it (the close the user just asked for).
        deliver(d, TermMessage.Sessions(true, emptyList()))
        assertEquals(
            emptyList(),
            d.shells.value.entries,
            "a pushed list is authoritative: the row must go without a refresh",
        )
        assertNull(d.shells.value.current, "and the shell it pointed at is gone")
    }

    /// A pushed list spanning records is still one answer: no half-set is applied.
    @Test
    fun `a pushed list is complete only at its last page`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11"))))
        deliver(d, profile, rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))
        deliver(d, TermMessage.Sessions(false, listOf(ent("bb22"))))
        assertEquals(listOf("aa11"), d.shells.value.entries.map { it.id }, "one page is not the answer")
        deliver(d, TermMessage.Sessions(true, listOf(ent("cc33"))))
        assertEquals(listOf("bb22", "cc33"), d.shells.value.entries.map { it.id })
    }

    @Test
    fun `a list that comes back empty is no shells`() = runBlocking<Unit> {
        val d = driver(); probed(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS), TermMessage.Sessions(true, emptyList()))
        assertEquals(MeshTermView.NoShells, d.view.value)
    }

    @Test
    fun `a Sessions nobody asked for attaches nothing`() = runBlocking<Unit> {
        val d = driver(); live(d)
        val before = sess.messages().size
        deliver(d, TermMessage.Sessions(true, listOf(ent("zz"))))
        assertEquals(before, sess.messages().size)
        assertIs<MeshTermView.Live>(d.view.value)
    }

    @Test
    fun `a named shell that is ambiguous or missing still ends visibly, with no picker`() = runBlocking<Unit> {
        val d = MeshTermDriver(gw, { hs }, selector = "ab", clock = { now })
        probed(d)
        assertEquals(TermMessage.Attach(MeshTerm.VERSION, "ab"), sess.messages().single())
        deliver(d, TermMessage.Closed(MeshTerm.Closed.AMBIGUOUS))
        assertEquals(MeshTermView.HostClosed(MeshTerm.Closed.AMBIGUOUS), d.view.value)
    }

    /// **A resume in flight is not "nothing is happening", and asking for the list kills it.**
    /// The screen's list retry keys on "not live and nothing holding a picture", which a resume is
    /// until its `Profile` arrives; the retry's `launcher()` queues a `Detach` beside the attach and
    /// the host drops the shell, leaving a page that paints but cannot be typed into.
    @Test
    fun `a named attach in flight reports attaching, and stops once painted`() = runBlocking<Unit> {
        val d = MeshTermDriver(gw, { hs }, selector = "aa11", clock = { now })
        probed(d)
        assertTrue(d.attaching, "the resume attach is on the wire")
        deliver(d, profile, rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))
        assertFalse(d.attaching, "described: it is a page now, not an attach")
    }

    // ── the header's shell list, and moving between shells ─────────────────────────────

    @Test
    fun `listing while attached fills the header and attaches nothing`() = runBlocking<Unit> {
        val d = driver(); live(d)
        val before = sess.messages().size
        d.listShells()
        assertEquals(TermMessage.ListRequest, sess.messages().last())
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11", "RCNT"), ent("bb22", "pi"))))
        assertEquals(before + 1, sess.messages().size, "only the ListRequest was sent")
        assertEquals(listOf(ent("aa11", "RCNT"), ent("bb22", "pi")), d.shells.value.entries)
        assertEquals(null, d.shells.value.current, "an empty-selector attach to one shell does not say which")
        assertIs<MeshTermView.Live>(d.view.value)
    }

    @Test
    fun `exactly one listed shell is the one that was attached`() = runBlocking<Unit> {
        val d = driver(); live(d)
        d.listShells()
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11", "RCNT"))))
        assertEquals("aa11", d.shells.value.current)
    }

    @Test
    fun `selecting a listed shell sends one Attach and drops the old screen at once`() = runBlocking<Unit> {
        val d = driver(); live(d)
        d.listShells()
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11"), ent("bb22"))))
        assertEquals(true, d.selectShell("bb22"))
        assertEquals(TermMessage.Attach(MeshTerm.VERSION, "bb22"), sess.messages().last())
        assertEquals("bb22", d.shells.value.current)
        assertEquals(MeshTermView.ProfileMissing, d.view.value, "no old rows under the new name")
        deliver(d, profile, rows(1, true, 0 to "new> "), TermMessage.Commit(1, 5, 0, 0))
        assertEquals("new> ", (d.view.value as MeshTermView.Live).screen.lines[0])
    }

    @Test
    fun `updates of the old shell arriving after a switch are ignored, not answered with a Resync`() = runBlocking<Unit> {
        val d = driver(); live(d)
        d.listShells()
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11"), ent("bb22"))))
        d.selectShell("bb22")
        val sent = sess.messages().size
        deliver(d, rows(2, false, 0 to "stale"), TermMessage.Commit(2, 0, 0, 0))
        assertEquals(sent, sess.messages().size, "no Resync for a screen we no longer have")
        assertEquals(MeshTermView.ProfileMissing, d.view.value)
    }

    @Test
    fun `an id the host did not list, and the shell already attached, send nothing`() = runBlocking<Unit> {
        val d = driver(); live(d)
        d.listShells()
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11"), ent("bb22"))))
        d.selectShell("bb22")
        val sent = sess.messages().size
        assertEquals(false, d.selectShell("zz99"))
        assertEquals(false, d.selectShell("bb22"))
        assertEquals(sent, sess.messages().size)
    }

    @Test
    fun `nothing can be selected before the host has listed`() = runBlocking<Unit> {
        val d = driver(); live(d)
        val sent = sess.messages().size
        assertEquals(false, d.selectShell("aa11"))
        assertEquals(sent, sess.messages().size)
    }

    @Test
    fun `the picker states are different values from every other not-working state`() {
        val states = listOf(
            MeshTermView.Listing, MeshTermView.Choose(listOf(ent("a"))), MeshTermView.NoShells,
            MeshTermView.ProfileMissing, MeshTermView.Handshaking,
        )
        assertEquals(states.size, states.map { meshTermLabel(it) }.toSet().size)
    }

    @Test
    fun `the three failure cases are three different values`() = runBlocking<Unit> {
        val down = MeshTermView.LinkDown(MeshLinkState.RadioUnavailable)
        val missing = MeshTermView.ProfileMissing
        val closed = MeshTermView.HostClosed(MeshTerm.Closed.EXITED)
        assertEquals(3, setOf(down, missing, closed).size)
        assertNotEquals<MeshTermView>(down, missing)
    }

    @Test
    fun `a profile and a commit make a live screen, with every absent feature named`() = runBlocking<Unit> {
        val d = driver(); live(d)
        val v = assertIs<MeshTermView.Live>(d.view.value)
        assertEquals(listOf("$ ", "", ""), v.screen.lines)
        assertEquals(2, v.screen.cursorX)
        assertTrue(v.painted)
        assertEquals(MeshTermFeature.entries.toList(), v.absent)
        assertEquals(58, v.budgetLeft)
    }

    @Test
    fun `rows without their commit are never painted`() = runBlocking<Unit> {
        val d = driver(); d.start(); d.onRecord(byteArrayOf(2))
        deliver(d, profile, rows(1, true, 0 to "half"))
        val v = assertIs<MeshTermView.Live>(d.view.value)
        assertEquals(listOf("", "", ""), v.screen.lines)
        assertTrue(!v.painted)
        deliver(d, TermMessage.Commit(1, 0, 0, 0))
        assertEquals("half", assertIs<MeshTermView.Live>(d.view.value).screen.lines[0])
    }

    @Test
    fun `input is refused until live and goes out as one Input message after`() = runBlocking<Unit> {
        val d = driver()
        assertEquals(InputOutcome.NotLive, d.input(byteArrayOf(3)))
        d.start(); d.onRecord(byteArrayOf(2))
        assertEquals(InputOutcome.NotLive, d.input(byteArrayOf(3)), "attached but no profile: still not ours to type into")
        deliver(d, profile, rows(1, true), TermMessage.Commit(1, 0, 0, 0))
        assertEquals(InputOutcome.Accepted, d.input("ls\r".encodeToByteArray()))
        assertEquals(TermMessage.Input("ls\r".encodeToByteArray()), sess.messages().last())
        assertEquals(InputOutcome.TooLong, d.input(ByteArray(MeshTerm.MAX_INPUT + 1)))
        assertEquals(InputOutcome.Empty, d.input(ByteArray(0)))
        sess.queueOutcome = QueueOutcome.Full
        assertEquals(InputOutcome.QueueFull, d.input(byteArrayOf(1)))
    }

    @Test
    fun `keys go out as one Key message of names, never as bytes, and only while live`() = runBlocking<Unit> {
        val d = driver()
        assertEquals(InputOutcome.NotLive, d.keys(listOf("Up")))
        d.start(); d.onRecord(byteArrayOf(2))
        assertEquals(InputOutcome.NotLive, d.keys(listOf("Up")), "attached but no profile")
        deliver(d, profile, rows(1, true), TermMessage.Commit(1, 0, 0, 0))
        assertEquals(InputOutcome.Accepted, d.keys(listOf("Up", "S-Tab")))
        assertEquals(TermMessage.Key(listOf("Up", "S-Tab")), sess.messages().last())
        assertEquals(InputOutcome.Empty, d.keys(emptyList()))
        assertEquals(InputOutcome.TooLong, d.keys(listOf("not a name")), "a space is the separator, so it is refused")
        assertEquals(InputOutcome.TooLong, d.keys(List(MeshTerm.MAX_KEYS + 1) { "Up" }))
        sess.queueOutcome = QueueOutcome.Full
        assertEquals(InputOutcome.QueueFull, d.keys(listOf("Up")))
    }

    @Test
    fun `the profile names the attached shell, so the header knows it before any list`() = runBlocking<Unit> {
        val d = driver(); d.start(); d.onRecord(byteArrayOf(2))
        assertEquals(null, d.shells.value.current)
        deliver(d, TermMessage.Profile(2, 20, 3, 0, "bb22"), rows(1, true), TermMessage.Commit(1, 0, 0, 0))
        assertEquals("bb22", d.shells.value.current)
        // A version-1 host names nothing, and the marker stays unknown rather than guessed.
        val old = driver(); old.start(); old.onRecord(byteArrayOf(2))
        deliver(old, TermMessage.Profile(1, 20, 3, 0), rows(1, true), TermMessage.Commit(1, 0, 0, 0))
        assertEquals(null, old.shells.value.current)
    }

    @Test
    fun `a commit that skips an epoch is discarded and answered with Resync`() = runBlocking<Unit> {
        val d = driver(); live(d)
        deliver(d, rows(3, false, 0 to "ghost"), TermMessage.Commit(3, 0, 0, 0))
        val v = assertIs<MeshTermView.Live>(d.view.value)
        assertEquals("$ ", v.screen.lines[0], "the old screen stays; a diff over a missing base is not applied")
        assertTrue(v.resyncing)
        assertEquals(TermMessage.Resync, sess.messages().last())
        // The host answers with a full screen and the mirror converges.
        deliver(d, profile, rows(4, true, 0 to "fresh"), TermMessage.Commit(4, 0, 0, 0))
        val v2 = assertIs<MeshTermView.Live>(d.view.value)
        assertEquals("fresh", v2.screen.lines[0]); assertTrue(!v2.resyncing)
    }

    @Test
    fun `only one Resync is outstanding at a time`() = runBlocking<Unit> {
        val d = driver(); live(d)
        deliver(d, rows(5, false), TermMessage.Commit(5, 0, 0, 0))
        deliver(d, rows(6, false), TermMessage.Commit(6, 0, 0, 0))
        assertEquals(1, sess.messages().count { it == TermMessage.Resync })
    }

    @Test
    fun `an in-order diff applies`() = runBlocking<Unit> {
        val d = driver(); live(d)
        deliver(d, rows(2, false, 1 to "a.txt"), TermMessage.Commit(2, 0, 1, 0))
        assertEquals(listOf("$ ", "a.txt", ""), assertIs<MeshTermView.Live>(d.view.value).screen.lines)
    }

    @Test
    fun `a malformed message costs one message and asks for a resync`() = runBlocking<Unit> {
        val d = driver(); live(d)
        sess.nextReceive = Received.Data(listOf(Delivered(0, byteArrayOf(0x83.toByte(), 1, 0))))
        d.onRecord(byteArrayOf(9))
        assertEquals(TermMessage.Resync, sess.messages().last())
        assertIs<MeshTermView.Live>(d.view.value)
    }

    @Test
    fun `a stream that cannot be trusted ends the attach visibly`() = runBlocking<Unit> {
        val d = driver(); live(d)
        // A body one byte past the message bound (600), so the stream is unusable rather than merely
        // malformed. The bound is the fixture's, not a literal repeated here.
        val over = MeshTerm.MAX_BODY + 1
        sess.nextReceive = Received.Data(
            listOf(Delivered(0, byteArrayOf(0x82.toByte(), (over and 0x7f or 0x80).toByte(), (over shr 7).toByte())))
        )
        d.onRecord(byteArrayOf(9))
        assertEquals(MeshTermView.LinkDown(MeshLinkState.SessionFailed), d.view.value)
        assertEquals(InputOutcome.NotLive, d.input(byteArrayOf(1)))
    }

    @Test
    fun `a shell that ended goes back to the list, not to a dead link`() = runBlocking<Unit> {
        // The rule this used to assert (EXITED means HostClosed, which stops every later record) was
        // wrong for the one case it actually happens in: the session is alive and the host still has
        // other shells. Quitting a shell at a desk returns you to the launcher there; here it must
        // return to the list, or leaving a shell costs a whole handshake to see the others again.
        val d = driver(); live(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.EXITED))
        assertEquals(MeshTermView.Listing, d.view.value)
        assertEquals(InputOutcome.NotLive, d.input(byteArrayOf(1)))
    }

    @Test
    fun `a close that is not an ending still stops the attach`() = runBlocking<Unit> {
        // VERSION is the phone and the host disagreeing about the wire, not a shell ending: there is
        // nothing to list, and the header's refresh is what starts a fresh handshake.
        val d = driver(); live(d)
        deliver(d, TermMessage.Closed(MeshTerm.Closed.VERSION))
        assertEquals(MeshTermView.HostClosed(MeshTerm.Closed.VERSION), d.view.value)
        assertEquals(InputOutcome.NotLive, d.input(byteArrayOf(1)))
    }

    @Test
    fun `the peer closing the session is a host-closed with no reason`() = runBlocking<Unit> {
        val d = driver(); live(d)
        sess.nextReceive = Received.PeerClosed
        d.onRecord(byteArrayOf(9))
        assertEquals(MeshTermView.HostClosed(null), d.view.value)
    }

    @Test
    fun `a record that fails a session check changes nothing and is counted`() = runBlocking<Unit> {
        val d = driver(); live(d)
        sess.nextReceive = Received.Rejected(SessionLib.ERR_BAD_TAG)
        d.onRecord(byteArrayOf(9))
        assertEquals(1, d.rejectedRecords)
        assertIs<MeshTermView.Live>(d.view.value)
    }

    @Test
    fun `the budget the gateway reports bounds what the session is asked for`() = runBlocking<Unit> {
        val d = driver(); live(d)
        gw.status.value = MeshStatus(0, null, 0, 2)
        sess.pollCalls.clear()
        repeat(5) { assertEquals(InputOutcome.Accepted, d.input(byteArrayOf(1))) }
        assertTrue(sess.pollCalls.all { it.first <= 2 && it.second <= 8 }, "poll asked for ${sess.pollCalls}")
    }

    @Test
    fun `an unreported budget does not mute the session`() = runBlocking<Unit> {
        // Silence is not "none". Treating it as zero stops the acks, the peer retransmits for
        // ever, and the screen reads "host stopped answering" while both radios are fine.
        gw.status.value = null
        val d = driver(); d.start(); d.onRecord(byteArrayOf(2))
        assertEquals(255, sess.pollCalls.last().first, "unlimited is the safe assumption")
    }

    @Test
    fun `a zero budget with something waiting is shown as a stall`() = runBlocking<Unit> {
        val d = driver(); live(d)
        gw.status.value = MeshStatus(0, null, 0, 0)
        sess.health = SessionHealth(false, false, false, pending = 3, dropped = 0)
        d.tick()
        assertTrue(assertIs<MeshTermView.Live>(d.view.value).stalled)
    }

    @Test
    fun `the radio going away is link-down and coming back restores the live screen`() = runBlocking<Unit> {
        val d = driver(); live(d)
        d.onGatewayState(MeshLinkState.RadioUnavailable)
        assertEquals(MeshTermView.LinkDown(MeshLinkState.RadioUnavailable), d.view.value)
        d.onGatewayState(MeshLinkState.Bound)
        assertIs<MeshTermView.Live>(d.view.value)
    }

    @Test
    fun `a refused destination is a visible failure and a busy radio is left to the ARQ`() = runBlocking<Unit> {
        val d = driver(); d.start()
        gw.outcome = MeshSendOutcome.ErrBusy
        d.onRecord(byteArrayOf(2))
        assertEquals(MeshTermView.ProfileMissing, d.view.value, "busy: the ARQ retransmits, no state change")
        val d2 = MeshTermDriver(gw, { FakeHandshake(FakeSession()) }, clock = { now })
        gw.outcome = MeshSendOutcome.ErrNotFound
        d2.start()
        assertEquals(MeshTermView.LinkDown(MeshLinkState.Refused), d2.view.value)
    }

    @Test
    fun `a handshake the host rejects is a refused link`() = runBlocking<Unit> {
        hs = FakeHandshake(sess, MeshSessionException(MeshLinkState.Refused, SessionLib.ERR_STALE, "stale"))
        val d = driver(); d.start(); d.onRecord(byteArrayOf(2))
        assertEquals(MeshTermView.LinkDown(MeshLinkState.Refused), d.view.value)
    }

    @Test
    fun `a gateway that closed ends the run so the caller can rebuild`() = runBlocking<Unit> {
        // The caller's only retry is a new connection handed to `run`; if a peer close leaves the
        // loop turning, the screen says "closed" for ever and no rebuild is ever reached.
        val d = driver()
        val job = launch { d.run() }
        gw.state.value = MeshLinkState.Closed
        withTimeout(5_000) { job.join() }
        assertEquals(MeshTermView.LinkDown(MeshLinkState.Closed), d.view.value)
    }

    @Test
    fun `a handshake nobody answers is re-asked, then ends as a dead transport`() = runBlocking<Unit> {
        // 8 s then 16 s then 32 s, three asks: a lost msg1 costs seconds, not the whole timeout, and
        // a host that is really gone ends the connection so the screen can build a new one. The
        // doubling matters - a fixed 6 s re-key raced its own answer on the real link.
        val d = driver(handshakeWaitMs = 120_000); d.start()
        val first = gw.sent.size
        now += 8_500; d.tick()
        assertEquals(first + 1, gw.sent.size, "the second ask at eight seconds")
        now += 16_500; d.tick()
        assertEquals(first + 2, gw.sent.size, "the third at sixteen after that")
        now += 32_500; d.tick()
        assertEquals(first + 3, gw.sent.size, "the fourth at thirty-two after that")
        // **The last re-ask gets its own window before the link is called dead.** It used to be
        // declared failed on the tick after it was sent, so the fourth attempt was never answered
        // by anything. The total is now the ladder's own sum: 8 + 16 + 32 + 64.
        now += 64_500; d.tick()
        assertEquals(MeshTermView.LinkDown(MeshLinkState.SessionFailed), d.view.value)
        assertTrue(hs.abandoned)
    }

    @Test
    fun `retries exhausted by the session is a session failure`() = runBlocking<Unit> {
        val d = driver(); live(d)
        sess.health = SessionHealth(failed = true, peerClosed = false, needsRekey = false, pending = 1, dropped = 0)
        d.tick()
        assertEquals(MeshTermView.LinkDown(MeshLinkState.SessionFailed), d.view.value)
    }

    @Test
    fun `a session out of nonce budget rekeys without taking the screen away`() = runBlocking<Unit> {
        val d = driver(); live(d)
        val before = d.view.value
        val sent = gw.sent.size
        sess.health = SessionHealth(false, false, needsRekey = true, pending = 0, dropped = 0)
        d.tick()
        // Not a failure: the link is up and the host is answering, so the screen must not say
        // the host stopped answering, and the wait must not back off.
        assertIs<MeshTermView.Handshaking>(d.view.value)
        assertTrue(before is MeshTermView.Live, "the test starts from a live screen")
        assertTrue(sess.freed, "the old session is closed, not kept")
        assertEquals(sent + 1, gw.sent.size, "a new handshake follows at once")
    }

    @Test
    fun `detach tells the host, asks the session to close and frees it`() = runBlocking<Unit> {
        val d = driver(); live(d)
        d.detach()
        assertEquals(TermMessage.Detach, sess.messages().last())
        assertTrue(sess.closedRequested && sess.freed)
        assertEquals(MeshTermView.Idle, d.view.value)
    }

    @Test
    fun `records for another shell number are not ours`() = runBlocking<Unit> {
        val d = driver(); live(d)
        sess.nextReceive = Received.Data(listOf(Delivered(7, stream(TermMessage.Closed(3)))))
        d.onRecord(byteArrayOf(9))
        assertIs<MeshTermView.Live>(d.view.value)
    }

    @Test
    fun `the driver's screen is exactly what Rust's Mirror held at every step of the scripted session`() = runBlocking<Unit> {
        val f = File("../../../crates/protocol/test/fixtures/mesh-term-vectors.json")
        val steps = Json.parseToJsonElement(f.readText()).jsonObject["script"]!!.jsonArray
        val d = driver(); d.start(); d.onRecord(byteArrayOf(2))
        for ((n, step) in steps.withIndex()) {
            val hex = step.jsonObject["stream"]!!.jsonPrimitive.content
            val bytes = ByteArray(hex.length / 2) { hex.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
            // Delivered in 144-byte pieces, as records would carry it.
            for (chunk in bytes.toList().chunked(MeshTerm.RECORD_PAYLOAD)) {
                sess.nextReceive = Received.Data(listOf(Delivered(0, chunk.toByteArray())))
                d.onRecord(byteArrayOf(9))
            }
            val want = step.jsonObject["mirror"]!!.jsonObject
            val v = assertIs<MeshTermView.Live>(d.view.value, "step $n")
            assertEquals(want["lines"]!!.jsonArray.map { it.jsonPrimitive.content }, v.screen.lines, "step $n")
            assertEquals(want["cursorX"]!!.jsonPrimitive.int, v.screen.cursorX)
            assertEquals(want["cursorY"]!!.jsonPrimitive.int, v.screen.cursorY)
            assertEquals(want["epoch"]!!.jsonPrimitive.int, v.screen.epoch)
            assertEquals(want["flags"]!!.jsonPrimitive.int and 1 != 0, v.screen.alternate)
            assertTrue(!v.resyncing, "step $n: a well-formed host stream never needs a resync")
        }
        assertTrue(sess.messages().none { it == TermMessage.Resync })
    }

    // ── the grid: what the phone asks for, and when ────────────────────────────────────────
    //
    // The bookkeeping is "what did we ask for", not "what did the host last say". The host's
    // `Profile` is an answer to a request already in flight, so reading it as "the host is
    // already the right size" throws away the request a rotation just made. Measured on the
    // device 2026-11-04: portrait → landscape → portrait inside one round trip left the pty at
    // the landscape width while the phone painted the portrait one.

    private fun resizes(d: MeshTermDriver) = sess.messages().filterIsInstance<TermMessage.Resize>()

    /** A live session whose host speaks version 3, so it can be asked to fit. */
    private suspend fun liveV3(d: MeshTermDriver) {
        d.start(); d.onRecord(byteArrayOf(2))
        deliver(d, TermMessage.Profile(3, 20, 10, 0), rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))
    }

    @Test
    fun `an attach carries the grid in the same record`() = runBlocking<Unit> {
        // **The grid is inside the attach, not a message after it.** A separate `Resize` cost the
        // host an `ATTACH_GRACE` wait for a number it already had, and a phone that stated nothing
        // got a repaint at the desk's geometry before its own: measured as **17 records to open a
        // shell**. One record, and one repaint at the right size.
        val d = driver()
        d.setGrid(46, 17)
        d.start(); d.onRecord(byteArrayOf(2))
        // The shell has to be one the phone has been told about, which is how `open` refuses an id
        // it has never heard of.
        deliver(d, TermMessage.Sessions(true, listOf(ent("abcd-1"))))
        sess.queued.clear()
        d.open("abcd-1")
        assertEquals(1, sess.queued.size, "one record: ${sess.queued.map { it.second.size }}")
        assertEquals(
            listOf<TermMessage>(TermMessage.Attach(MeshTerm.VERSION, "abcd-1", 46, 17)),
            sess.messages(),
        )
    }

    /// **Two things done at once are one record.**
    ///
    /// Closing a shell also asks for the list, and releasing a hold rides with the line it was
    /// holding: each pair used to pay a whole record of air for the second half, which on this link
    /// is 0.6 s. The driver packs every message it queues, not only the pairs someone wrapped in
    /// `queueAll`.
    @Test
    fun `a close and the list it asks for travel as one record`() = runBlocking<Unit> {
        val d = driver()
        d.start(); d.onRecord(byteArrayOf(2))
        deliver(d, TermMessage.Sessions(true, listOf(ent("aa11"), ent("bb22"))))
        sess.queued.clear()
        assertTrue(d.closeShell("aa11"))
        assertEquals(1, sess.queued.size, "one record: ${sess.queued.map { it.second.size }}")
        val sent = sess.messages()
        assertTrue(sent.contains(TermMessage.Close("aa11")), "the close is in it: $sent")
        assertTrue(sent.contains(TermMessage.ListRequest), "and so is the list: $sent")
    }

    @Test
    fun `a rotation back to a grid the host has not answered yet still asks`() = runBlocking<Unit> {
        val d = driver()
        liveV3(d)                                 // host profile: 20x10
        d.setGrid(30, 10)                         // rotate to landscape
        assertEquals(listOf(TermMessage.Resize(30, 10)), resizes(d))
        // Back to portrait before the host's answer arrives: the profile still says 20x10, which
        // is exactly the grid being asked for. Comparing against it would send nothing, and the
        // host would finish resizing to 30x10 under a phone painting 20x10.
        d.setGrid(20, 10)
        assertEquals(
            listOf(TermMessage.Resize(30, 10), TermMessage.Resize(20, 10)),
            resizes(d),
            "the second rotation is a request of its own",
        )
    }

    @Test
    fun `the same grid for the same shell is asked for once`() = runBlocking<Unit> {
        val d = driver()
        liveV3(d)
        d.setGrid(30, 10)
        d.setGrid(30, 10)
        d.setGrid(30, 10)
        assertEquals(1, resizes(d).size, "a host that clamps it cannot start a loop")
    }

    @Test
    fun `a resize the session refused to queue is asked again on the next tick`() = runBlocking<Unit> {
        val d = driver()
        liveV3(d)
        sess.queueOutcome = QueueOutcome.Full
        d.setGrid(30, 10)
        assertTrue(resizes(d).isEmpty(), "a full queue takes nothing")
        sess.queueOutcome = QueueOutcome.Queued
        d.tick()
        assertEquals(
            listOf(TermMessage.Resize(30, 10)),
            resizes(d),
            "the request is retried rather than lost for the life of the page",
        )
    }

    // ── the transport's word ends the connection ────────────────────────────────────────
    //
    // Every failure below used to set the view and *keep looping*, so `run` never returned and the
    // screen's rebuild — the retry that cures a transient refusal — was never reached. That is the
    // reported "the host is not connected or out of range", cured by one refresh and working in
    // two seconds: the refresh built the connection the driver had stopped building.

    @Test
    fun `a radio that refuses the record ends the connection so it can be rebuilt`() = runBlocking<Unit> {
        val d = driver()
        live(d)
        gw.outcome = MeshSendOutcome.ErrNotFound
        d.input("x".toByteArray())
        assertIs<MeshTermView.LinkDown>(d.view.value)
        assertTrue(sess.freed, "the session is dropped, not kept over a transport that will not carry")
        assertTrue(withTimeout(5_000) { d.run(); true }, "run returns, so the caller can rebuild")
    }

    @Test
    fun `a session that throws while polling ends the connection`() = runBlocking<Unit> {
        val d = driver()
        live(d)
        sess.failPoll = MeshSessionException(MeshLinkState.SessionFailed, -1, "gone")
        d.tick()
        assertIs<MeshTermView.LinkDown>(d.view.value)
        assertTrue(withTimeout(5_000) { d.run(); true }, "run returns rather than ticking over a dead session")
    }

    @Test
    fun `a handshake record that cannot be finished ends the connection`() = runBlocking<Unit> {
        hs = FakeHandshake(sess, MeshSessionException(MeshLinkState.SessionFailed, -1, "bad msg2"))
        val d = driver()
        d.start()
        d.onRecord(byteArrayOf(7))
        assertIs<MeshTermView.LinkDown>(d.view.value)
        assertTrue(withTimeout(5_000) { d.run(); true }, "run returns rather than retrying over the same transport")
    }

    @Test
    fun `a list already in flight is not asked for again`() = runBlocking<Unit> {
        val d = driver()
        live(d)
        val before = sess.messages().count { it is TermMessage.ListRequest }
        d.listShells()
        d.listShells()
        d.listShells()
        assertEquals(
            1,
            sess.messages().count { it is TermMessage.ListRequest } - before,
            "the screen asks once per frame until the answer lands; each ask is a record",
        )
    }
}
