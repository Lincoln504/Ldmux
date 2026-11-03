package dev.ldmux.mobile.mesh

import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import dev.ldmux.mobile.MeshKeyMaterial
import dev.ldmux.mobile.Pairing
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * The attach driver over the **real Rust session** (through the JNI shim) with the daemon's end
 * played by `responderAccept` and a small stand-in for the shell bridge speaking the profile.
 * The gateway is the only fake: it hands each record straight to the other end. Proves the
 * driver, the codec and the real session agree on bytes; it does not prove a radio or a device.
 */
class MeshTermIntegrationTest {
    private val wall = 1_800_000_000_000L
    private var now = 10_000L

    private class Gateway : GatewayPort {
        override val state = MutableStateFlow(MeshLinkState.Bound)
        override val status = MutableStateFlow<MeshStatus?>(MeshStatus(0, null, null, 0, 58))
        override val received = MutableSharedFlow<ByteArray>(extraBufferCapacity = 8)
        override val uplinkRecordMax = 172
        val sent = ArrayList<ByteArray>()
        override suspend fun send(record: ByteArray): MeshSendResult {
            sent += record
            return MeshSendResult(0, MeshSendOutcome.SentDirect)
        }
    }

    private fun stream(vararg m: TermMessage): ByteArray = m.flatMap { it.encode().toList() }.toByteArray()

    @Test
    fun `attach, screen, keystroke, diff and a forged record, over the real session`() = runBlocking<Unit> {
        val lib = assertNotNull(loadSessionLib(), "build the host library first (native/build-session.sh host)")
        val (hostPriv, hostPub) = MeshSession.generateKeypair(lib)
        val (phonePriv, phonePub) = MeshSession.generateKeypair(lib)
        val psk = ByteArray(32) { 5 }
        val gw = Gateway()
        val driver = MeshTermDriver(
            gw,
            begin = { MeshSession.begin(lib, phonePriv, phonePub, hostPub, psk, wall) },
            clock = { now },
        )

        // --- handshake: message 1 goes out as the first record, the host answers with message 2.
        driver.start()
        assertEquals(MeshTermView.Handshaking, driver.view.value)
        val msg1 = gw.sent.removeAt(0)
        val reply = ByteArray(SessionLib.MSG2_LEN)
        val out = LongArray(2)
        assertEquals(
            SessionLib.OK,
            lib.responderAccept(hostPriv, hostPub, psk, null, 0, wall, msg1, 3, 172, reply, ByteArray(32), out),
        )
        val host = MeshSession(lib, out[0])
        driver.onRecord(reply)
        assertEquals(MeshTermView.ProfileMissing, driver.view.value)

        // --- the host receives the sealed Attach and describes a 20x3 terminal.
        fun toHost(): List<Delivered> {
            val got = ArrayList<Delivered>()
            for (rec in gw.sent) (host.receive(now, rec) as? Received.Data)?.let { got += it.chunks }
            gw.sent.clear()
            return got
        }
        val attach = toHost().single()
        assertEquals(listOf<TermItem>(TermItem.Message(TermMessage.Attach(MeshTerm.VERSION, ""))), TermDecoder().push(attach.bytes))

        fun hostSays(vararg m: TermMessage): List<ByteArray> {
            assertEquals(QueueOutcome.Queued, host.send(0, stream(*m)))
            return host.poll(now, 20, 8)
        }
        suspend fun deliver(records: List<ByteArray>) { for (r in records) driver.onRecord(r) }

        deliver(hostSays(
            TermMessage.Profile(1, 20, 3, 0x7f),
            TermMessage.Rows(1, true, listOf(TermPatch(0, false, "$ "))),
            TermMessage.Commit(1, 2, 0, 0),
        ))
        var v = assertIs<MeshTermView.Live>(driver.view.value)
        assertEquals(listOf("$ ", "", ""), v.screen.lines)
        assertEquals(MeshTermFeature.entries.toList(), v.absent)

        // --- a keystroke line goes out sealed and the host reads exactly the Input message.
        assertEquals(InputOutcome.Accepted, driver.input("ls\r".encodeToByteArray()))
        val typed = toHost().single()
        assertEquals(listOf<TermItem>(TermItem.Message(TermMessage.Input("ls\r".encodeToByteArray()))), TermDecoder().push(typed.bytes))

        // --- the host answers with a diff.
        deliver(hostSays(
            TermMessage.Rows(2, false, listOf(TermPatch(0, false, "$ ls"), TermPatch(1, false, "a.txt"))),
            TermMessage.Commit(2, 0, 2, 0),
        ))
        v = assertIs<MeshTermView.Live>(driver.view.value)
        assertEquals(listOf("$ ls", "a.txt", ""), v.screen.lines)

        // --- a forged record changes nothing and is counted.
        val good = hostSays(TermMessage.Rows(3, false, listOf(TermPatch(2, false, "x"))), TermMessage.Commit(3, 0, 2, 0))
        val forged = good[0].copyOf().also { it[it.size - 1] = (it[it.size - 1].toInt() xor 1).toByte() }
        driver.onRecord(forged)
        assertEquals(1, driver.rejectedRecords)
        assertEquals(listOf("$ ls", "a.txt", ""), assertIs<MeshTermView.Live>(driver.view.value).screen.lines)
        deliver(good)
        assertEquals(listOf("$ ls", "a.txt", "x"), assertIs<MeshTermView.Live>(driver.view.value).screen.lines)

        // --- no budget, nothing on the air: input is accepted into the session but not transmitted.
        gw.status.value = MeshStatus(0, null, null, 0, 0)
        assertEquals(InputOutcome.Accepted, driver.input(byteArrayOf(0x03)))
        assertTrue(gw.sent.isEmpty(), "a zero budget transmits nothing: ${gw.sent.size} records left the phone")
        // The budget returns and the held record goes out.
        gw.status.value = MeshStatus(0, null, null, 0, 5)
        now += 1
        driver.tick()
        assertEquals(1, gw.sent.size)

        // --- the shell ends: the phone asks for the list again on the same session, and input is
        // not accepted until it has attached one. A closed shell is not a closed link.
        deliver(hostSays(TermMessage.Closed(MeshTerm.Closed.EXITED)))
        assertEquals(MeshTermView.Listing, driver.view.value)
        assertEquals(InputOutcome.NotLive, driver.input(byteArrayOf(1)))
        driver.detach(); host.close()
    }
}
