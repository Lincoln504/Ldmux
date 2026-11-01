package dev.ldmux.mobile.mesh

import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.io.File
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNotEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.test.assertTrue

/** The driver against fakes: no socket, no native library. Every rule it owns has a case. */
class MeshTermDriverTest {
    private class FakeGateway : GatewayPort {
        override val state = MutableStateFlow(MeshLinkState.Bound)
        override val status = MutableStateFlow<MeshStatus?>(MeshStatus(0, null, null, 0, 58))
        override val received = MutableSharedFlow<ByteArray>(extraBufferCapacity = 8)
        override var uplinkRecordMax = 172
        val sent = ArrayList<ByteArray>()
        var outcome = MeshSendOutcome.SentDirect
        /** A gateway that takes the record and never answers. */
        var silent = false
        override suspend fun send(record: ByteArray): MeshSendResult {
            sent += record
            if (silent) kotlinx.coroutines.awaitCancellation()
            return MeshSendResult(0, outcome)
        }
        override fun close() {}
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
        override fun poll(nowMs: Long, recordsLeft: Int, l2Slots: Int): List<ByteArray> {
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
    private fun driver(handshakeTimeoutMs: Long = 300_000, sendTimeoutMs: Long = 10_000) =
        MeshTermDriver(gw, { hs }, selector = "", clock = { now }, handshakeTimeoutMs = handshakeTimeoutMs, sendTimeoutMs = sendTimeoutMs)

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
    fun a_dead_session_is_retried_with_a_new_handshake(): Unit = runBlocking {
        val d = driver()
        live(d)
        val before = gw.sent.size
        sess.health = SessionHealth(failed = true, peerClosed = false, needsRekey = false, pending = 3, dropped = 0)
        d.tick()
        assertIs<MeshTermView.LinkDown>(d.view.value, "the screen says the host stopped answering")
        assertTrue(sess.freed, "the dead session is dropped, not kept")
        val wake = d.nextWakeMs()
        assertEquals(now + 2_000, wake, "the first retry is two seconds later")
        now = wake!!
        d.tick()
        assertEquals(before + 1, gw.sent.size, "the retry is a new handshake message 1")
        assertIs<MeshTermView.Handshaking>(d.view.value)
    }

    /** The wait doubles, so a host that is off is not hammered with airtime. */
    @Test
    fun the_retry_backs_off_and_stops_growing(): Unit = runBlocking {
        var dead = false
        val d = MeshTermDriver(
            gw,
            { if (dead) throw MeshSessionException(MeshLinkState.SessionFailed, 1, "gone") else hs },
            selector = "",
            clock = { now },
        )
        live(d)
        sess.health = SessionHealth(failed = true, peerClosed = false, needsRekey = false, pending = 1, dropped = 0)
        dead = true
        val waits = ArrayList<Long>()
        repeat(6) {
            d.tick()
            val wake = d.nextWakeMs() ?: error("a retry must be scheduled")
            waits += wake - now
            now = wake
        }
        assertEquals(listOf(2_000L, 4_000L, 8_000L, 16_000L, 30_000L, 30_000L), waits)
    }

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
    }

    private suspend fun live(d: MeshTermDriver) {
        d.start(); d.onRecord(byteArrayOf(2))
        deliver(d, profile, rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))
    }

    @Test
    fun `an unbound gateway is a visible link-down and nothing is sent`() = runBlocking<Unit> {
        gw.state.value = MeshLinkState.Unreachable
        val d = driver(); d.start()
        assertEquals(MeshTermView.LinkDown(MeshLinkState.Unreachable), d.view.value)
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

    private fun ent(id: String, label: String = "") = SessionEntry(id, label)
    private suspend fun probed(d: MeshTermDriver) { d.start(); d.onRecord(byteArrayOf(2)) }

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
        assertEquals(false, d.choose("aa11"), "a second choice after attaching is refused")
        deliver(d, profile, rows(1, true, 0 to "$ "), TermMessage.Commit(1, 2, 0, 0))
        assertIs<MeshTermView.Live>(d.view.value)
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
        val down = MeshTermView.LinkDown(MeshLinkState.Unreachable)
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
        sess.nextReceive = Received.Data(listOf(Delivered(0, byteArrayOf(0x82.toByte(), 0xad.toByte(), 0x02))))
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
        gw.status.value = MeshStatus(0, null, null, 0, 2)
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
        gw.status.value = MeshStatus(0, null, null, 0, 0)
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
    fun `a handshake nobody answers ends as a dead session after the timeout`() = runBlocking<Unit> {
        val d = driver(handshakeTimeoutMs = 60_000); d.start()
        now += 59_000; d.tick()
        assertEquals(MeshTermView.Handshaking, d.view.value)
        now += 2_000; d.tick()
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
}
