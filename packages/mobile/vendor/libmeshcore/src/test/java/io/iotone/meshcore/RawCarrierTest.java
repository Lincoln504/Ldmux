// Copyright (c) 2026 IoTone, Inc.
// SPDX-License-Identifier: MIT
package io.iotone.meshcore;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.iotone.meshcore.codec.DecodeErrorKind;
import io.iotone.meshcore.codec.MeshcoreFrameCodec;
import io.iotone.meshcore.frames.DecodeFailure;
import io.iotone.meshcore.frames.RawDataFrame;
import io.iotone.meshcore.model.Contact;

import org.junit.jupiter.api.Test;

/** Raw-custom path encoding, packet escape hatch, and the 0x84 marker. */
class RawCarrierTest {

    @Test
    void rawDataUsesEncodedPathLengthNotByteCount() {
        byte[] frame = MeshcoreFrameCodec.sendRawData(
                0x41, new byte[] {(byte) 0xAA, (byte) 0xBB},
                new byte[] {1, 2, 3, 4});
        assertArrayEquals(
                TestUtil.bytes(0x19, 0x41, 0xAA, 0xBB, 1, 2, 3, 4), frame);
    }

    @Test
    void rawDataRejectsMismatchedPathEncoding() {
        org.junit.jupiter.api.Assertions.assertThrows(
                IllegalArgumentException.class,
                () -> MeshcoreFrameCodec.sendRawData(
                        1, new byte[] {0x11, 0x22}, new byte[] {1, 2, 3, 4}));
    }

    @Test
    void rawDataRequiresFourPayloadBytes() {
        org.junit.jupiter.api.Assertions.assertThrows(
                IllegalArgumentException.class,
                () -> MeshcoreFrameCodec.sendRawData(
                        0, new byte[0], new byte[] {1, 2, 3}));
    }

    @Test
    void rawDataZeroHopStillWorks() {
        byte[] frame = MeshcoreFrameCodec.sendRawData(
                0, new byte[0], new byte[] {1, 2, 3, 4});
        assertArrayEquals(TestUtil.bytes(0x19, 0x00, 1, 2, 3, 4), frame);
    }

    @Test
    void rawPacketCarriesTheWholePacket() {
        byte[] packet = new byte[] {0x3E, 0x00, 1, 2, 3, 4};
        byte[] frame = MeshcoreFrameCodec.sendRawPacket(0, packet);
        assertArrayEquals(
                TestUtil.bytes(0x41, 0x00, 0x3E, 0x00, 1, 2, 3, 4), frame);
    }

    @Test
    void rawPacketRefusesOversizePacket() {
        byte[] tooBig = new byte[MeshcoreConstants.MAX_RAW_PACKET + 1];
        org.junit.jupiter.api.Assertions.assertThrows(
                IllegalArgumentException.class,
                () -> MeshcoreFrameCodec.sendRawPacket(0, tooBig));
    }

    @Test
    void pathHashModeUsesFirmwareModeNotSize() {
        assertArrayEquals(TestUtil.bytes(0x3D, 0x00, 0x00),
                MeshcoreFrameCodec.setPathHashMode(0));
        assertArrayEquals(TestUtil.bytes(0x3D, 0x00, 0x02),
                MeshcoreFrameCodec.setPathHashMode(2));
        org.junit.jupiter.api.Assertions.assertThrows(
                IllegalArgumentException.class,
                () -> MeshcoreFrameCodec.setPathHashMode(3));
    }

    @Test
    void rawDataMarkerMustBeFF() {
        byte[] good = TestUtil.bytes(0x84, 0, 0, 0xFF, 1, 2, 3, 4);
        RawDataFrame frame = assertInstanceOf(RawDataFrame.class,
                MeshcoreFrameCodec.decode(good));
        assertArrayEquals(new byte[] {1, 2, 3, 4}, frame.payload());

        byte[] bad = TestUtil.bytes(0x84, 0, 0, 0x00, 1, 2, 3, 4);
        DecodeFailure failure = assertInstanceOf(DecodeFailure.class,
                MeshcoreFrameCodec.decode(bad));
        assertEquals(DecodeErrorKind.MALFORMED, failure.error().kind());
    }

    @Test
    void rawDataPayloadMustNotBeEmptyAfterMarker() {
        DecodeFailure failure = assertInstanceOf(DecodeFailure.class,
                MeshcoreFrameCodec.decode(TestUtil.bytes(0x84, 0, 0, 0xFF)));
        assertEquals(DecodeErrorKind.MALFORMED, failure.error().kind());
    }

    @Test
    void getContactByKeyCarriesTheFullKey() {
        byte[] key = new byte[32];
        key[0] = 0x11;
        key[31] = (byte) 0xEE;
        byte[] frame = MeshcoreFrameCodec.getContactByKey(key);
        assertEquals(33, frame.length);
        assertEquals(0x1E, frame[0] & 0xFF);
        assertEquals(0x11, frame[1] & 0xFF);
        assertEquals(0xEE, frame[32] & 0xFF);
        org.junit.jupiter.api.Assertions.assertThrows(
                IllegalArgumentException.class,
                () -> MeshcoreFrameCodec.getContactByKey(new byte[31]));
    }

    @Test
    void contactPathLenIsDecodedAsHashCountAndSize() {
        byte[] path = new byte[64];
        path[0] = (byte) 0xAA;
        path[1] = (byte) 0xBB;
        Contact oneHopTwoByte = new Contact(
                new byte[32], 2, 0, 0x41, path, "r",
                0, 0, 0, 0);
        assertTrue(oneHopTwoByte.hasPath());
        assertEquals(2, oneHopTwoByte.pathHashSize());
        assertEquals(1, oneHopTwoByte.pathHopCount());
        assertEquals(2, oneHopTwoByte.pathByteCount());
        assertArrayEquals(new byte[] {(byte) 0xAA, (byte) 0xBB},
                oneHopTwoByte.activePath());

        Contact unknown = new Contact(
                new byte[32], 2, 0, Contact.OUT_PATH_UNKNOWN,
                new byte[64], "r", 0, 0, 0, 0);
        assertFalse(unknown.hasPath());
        assertFalse(unknown.hasValidPathEncoding());
        assertEquals(0, unknown.pathByteCount());
        assertEquals(0, unknown.activePath().length);

        Contact reserved = new Contact(
                new byte[32], 2, 0, 0xC0, new byte[64], "r", 0, 0, 0, 0);
        assertTrue(reserved.hasPath());
        assertFalse(reserved.hasValidPathEncoding());
        assertEquals(0, reserved.activePath().length);
    }

    @Test
    void contactPathLenThreeMeansThreeOneByteHops() {
        byte[] path = new byte[64];
        path[0] = 1;
        path[1] = 2;
        path[2] = 3;
        Contact c = new Contact(new byte[32], 2, 0, 3, path, "r",
                0, 0, 0, 0);
        assertEquals(1, c.pathHashSize());
        assertEquals(3, c.pathHopCount());
        assertArrayEquals(new byte[] {1, 2, 3}, c.activePath());
    }
}
