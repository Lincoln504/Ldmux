// Copyright (c) 2026 IoTone, Inc.
// SPDX-License-Identifier: MIT
package io.iotone.meshcore.android;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import io.iotone.meshcore.codec.FrameBuilder;
import io.iotone.meshcore.frames.MeshcoreInbound;
import io.iotone.meshcore.transport.MeshcoreTransport;

import org.junit.Test;

import java.io.IOException;
import java.util.ArrayList;
import java.util.List;

/**
 * The companion protocol has one uncorrelated answer slot. These tests pin the
 * two rules that keep it from being raced: only one tracked command is written
 * at a time, and a raw send waits until no tracked command is outstanding.
 */
public class MeshcoreSessionCommandGateTest {

    private static final class FakeTransport implements MeshcoreTransport {
        Listener listener;
        final List<byte[]> sent = new ArrayList<>();

        @Override
        public void setListener(Listener listener) {
            this.listener = listener;
        }

        @Override
        public boolean isConnected() {
            return true;
        }

        @Override
        public void send(byte[] frame) {
            sent.add(frame.clone());
        }

        @Override
        public void close() {
        }

        void deliver(byte[] frame) {
            listener.onFrame(frame.clone());
        }
    }

    private static final class Listener implements SessionListener {
        boolean ready;
        @Override public void onStateChanged(SessionState state) { }
        @Override public void onReady(io.iotone.meshcore.model.SelfInfo selfInfo) {
            ready = true;
        }
    }

    private static byte[] selfInfo() {
        byte[] pubkey = new byte[32];
        return new FrameBuilder()
                .u8(0x05)
                .u8(0x01)
                .u8(14)
                .u8(22)
                .raw(pubkey)
                .i32(0).i32(0)
                .u8(2).u8(1).u8(0).u8(1)
                .u32(910525)
                .u32(62500)
                .u8(7).u8(5)
                .utf8String("test")
                .build();
    }

    private static byte[] ok() {
        return new FrameBuilder().u8(0x00).build();
    }

    private MeshcoreSession readySession(FakeTransport transport, Listener listener) {
        MeshcoreSession session = new MeshcoreSession("test", transport, listener);
        session.setAutoDrain(false);
        transport.listener.onConnectionChanged(true);
        transport.deliver(selfInfo());
        assertTrue(listener.ready);
        return session;
    }

    @Test
    public void trackedCommandsAreWrittenOneAtATime() {
        FakeTransport transport = new FakeTransport();
        MeshcoreSession session = readySession(transport, new Listener());
        int before = transport.sent.size();

        session.requestContacts();
        session.setPathHashMode(1);
        session.setTxPower(20);
        assertEquals(before + 1, transport.sent.size());
        assertFalse(session.isIdle());
        assertEquals(0x04, transport.sent.get(before)[0] & 0xFF);

        transport.deliver(ok());
        assertEquals(before + 2, transport.sent.size());
        assertEquals(0x3D, transport.sent.get(before + 1)[0] & 0xFF);

        transport.deliver(ok());
        assertEquals(before + 3, transport.sent.size());
        assertEquals(0x0C, transport.sent.get(before + 2)[0] & 0xFF);

        transport.deliver(ok());
        assertTrue(session.isIdle());
        session.close();
    }

    @Test
    public void rawIsRefusedWhileATrackedCommandIsOutstanding() {
        FakeTransport transport = new FakeTransport();
        MeshcoreSession session = readySession(transport, new Listener());
        session.requestContacts();

        final boolean[] called = {false};
        final boolean[] ok = {false};
        boolean queued = session.sendRawData(
                0, new byte[0], new byte[] {1, 2, 3, 4},
                (result, error) -> { called[0] = true; ok[0] = result; });
        assertFalse(queued);
        assertTrue(called[0]);
        assertFalse(ok[0]);

        transport.deliver(ok());
        assertTrue(session.isIdle());
        session.close();
    }

    @Test
    public void aRawSendDefersTheNextTrackedCommandUntilItsAnswer() {
        FakeTransport transport = new FakeTransport();
        MeshcoreSession session = readySession(transport, new Listener());
        int before = transport.sent.size();

        final boolean[] answered = {false};
        assertTrue(session.sendRawData(
                0, new byte[0], new byte[] {1, 2, 3, 4},
                (result, error) -> answered[0] = true));
        session.setTxPower(20);
        assertEquals(before + 1, transport.sent.size());
        assertFalse(session.isIdle());

        transport.deliver(ok());
        assertTrue(answered[0]);
        assertEquals(before + 2, transport.sent.size());
        assertEquals(0x0C, transport.sent.get(before + 1)[0] & 0xFF);
        transport.deliver(ok());
        assertTrue(session.isIdle());
        session.close();
    }

    private static byte[] contact() {
        return new FrameBuilder()
                .u8(0x03)
                .raw(new byte[32])
                .u8(1).u8(0).u8(0)
                .raw(new byte[64])
                .raw(new byte[32])
                .u32(0).i32(0).i32(0).u32(0)
                .build();
    }

    @Test
    public void aSingleContactAnswerReleasesTheAnswerSlot() {
        FakeTransport transport = new FakeTransport();
        MeshcoreSession session = readySession(transport, new Listener());

        session.getContact(new byte[32]);
        assertFalse(session.isIdle());
        transport.deliver(contact());
        assertTrue(session.isIdle());
        session.close();
    }

    @Test
    public void aContactDumpStaysBusyUntilEndOfContacts() {
        FakeTransport transport = new FakeTransport();
        MeshcoreSession session = readySession(transport, new Listener());

        session.requestContacts();
        transport.deliver(contact());
        assertFalse(session.isIdle());
        transport.deliver(new FrameBuilder().u8(0x04).u32(0).build());
        assertTrue(session.isIdle());
        session.close();
    }
}
