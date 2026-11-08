// Copyright (c) 2026 IoTone, Inc.
// SPDX-License-Identifier: MIT
package io.iotone.meshcore.codec;

import io.iotone.meshcore.MeshcoreCommand;
import io.iotone.meshcore.MeshcoreConstants;
import io.iotone.meshcore.frames.AckFrame;
import io.iotone.meshcore.frames.AdvertFrame;
import io.iotone.meshcore.frames.AdvertPathFrame;
import io.iotone.meshcore.frames.BatteryStorageFrame;
import io.iotone.meshcore.frames.ChannelInfoFrame;
import io.iotone.meshcore.frames.ChannelMessageFrame;
import io.iotone.meshcore.frames.ContactFrame;
import io.iotone.meshcore.frames.ContactMessageFrame;
import io.iotone.meshcore.frames.ContactsStartFrame;
import io.iotone.meshcore.frames.CurrentTimeFrame;
import io.iotone.meshcore.frames.CustomVarsFrame;
import io.iotone.meshcore.frames.DecodeFailure;
import io.iotone.meshcore.frames.DefaultFloodScopeFrame;
import io.iotone.meshcore.frames.DeviceInfoFrame;
import io.iotone.meshcore.frames.EndOfContactsFrame;
import io.iotone.meshcore.frames.ErrorFrame;
import io.iotone.meshcore.frames.MeshcoreInbound;
import io.iotone.meshcore.frames.MessagesWaitingFrame;
import io.iotone.meshcore.frames.MsgSentFrame;
import io.iotone.meshcore.frames.NoMoreMessagesFrame;
import io.iotone.meshcore.frames.OkFrame;
import io.iotone.meshcore.frames.RawDataFrame;
import io.iotone.meshcore.frames.RfLogFrame;
import io.iotone.meshcore.frames.SelfInfoFrame;
import io.iotone.meshcore.frames.TelemetryResponseFrame;
import io.iotone.meshcore.frames.UnsupportedFrame;
import io.iotone.meshcore.model.Advert;
import io.iotone.meshcore.model.BatteryStorage;
import io.iotone.meshcore.model.ChannelInfo;
import io.iotone.meshcore.model.ChannelMessage;
import io.iotone.meshcore.model.Contact;
import io.iotone.meshcore.model.ContactMessage;
import io.iotone.meshcore.model.DeviceInfo;
import io.iotone.meshcore.model.MsgSent;
import io.iotone.meshcore.model.RadioParams;
import io.iotone.meshcore.model.RfLog;
import io.iotone.meshcore.model.SelfInfo;

import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.OptionalInt;
import java.util.OptionalLong;

/**
 * Encodes app&#8594;device command frames and decodes device&#8594;app
 * frames.
 *
 * <p>Frame = {@code [opcode][payload]}, one frame per BLE
 * write/notification, little-endian. All layouts are pinned (see
 * {@link MeshcoreConstants#FIRMWARE_PIN_TAG}). This class mirrors the
 * Dart reference codec byte-for-byte; both are validated against the
 * same JSON golden vectors.</p>
 *
 * <p>{@link #decode(byte[])} is <strong>total</strong>: it returns a
 * {@link DecodeFailure} for malformed or truncated input and an
 * {@link UnsupportedFrame} for opcodes this codec does not model. It
 * never throws.</p>
 */
public final class MeshcoreFrameCodec {

    private MeshcoreFrameCodec() {
        // Static codec only.
    }

    // ---------------------------------------------------------------------
    // Encoders (App → Device)
    // ---------------------------------------------------------------------

    /**
     * {@code CMD_APP_START} (0x01): {@code 01 [7 reserved] [app name]}.
     *
     * <p>Firmware requires {@code len >= 8} and ignores bytes 1..7. The
     * app name is the trailing UTF-8 bytes (no NUL terminator;
     * frame-length delimited).</p>
     *
     * @param appName application name announced to the device
     * @return the encoded frame
     */
    public static byte[] appStart(String appName) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.APP_START.code())
                .zeros(7)
                .utf8String(appName)
                .build();
    }

    /**
     * {@code CMD_GET_CONTACTS} (0x04), full dump.
     *
     * @return the encoded frame
     */
    public static byte[] getContacts() {
        return new FrameBuilder()
                .u8(MeshcoreCommand.GET_CONTACTS.code())
                .build();
    }

    /**
     * {@code CMD_GET_CONTACTS} (0x04), filtered to contacts modified
     * at/after {@code since} (uint32 unix seconds) for incremental sync.
     *
     * @param since lower bound on {@code lastMod}
     * @return the encoded frame
     */
    public static byte[] getContacts(long since) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.GET_CONTACTS.code())
                .u32(since)
                .build();
    }

    /**
     * {@code CMD_GET_DEVICE_TIME} (0x05).
     *
     * @return the encoded frame
     */
    public static byte[] getDeviceTime() {
        return new FrameBuilder()
                .u8(MeshcoreCommand.GET_DEVICE_TIME.code())
                .build();
    }

    /**
     * {@code CMD_SET_DEVICE_TIME} (0x06): {@code 06 [unix uint32 LE]}.
     *
     * @param unixSeconds time to set, unix seconds
     * @return the encoded frame
     */
    public static byte[] setDeviceTime(long unixSeconds) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SET_DEVICE_TIME.code())
                .u32(unixSeconds)
                .build();
    }

    /**
     * {@code CMD_SYNC_NEXT_MESSAGE} (0x0A) — pull the next queued
     * inbound message.
     *
     * @return the encoded frame
     */
    public static byte[] syncNextMessage() {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SYNC_NEXT_MESSAGE.code())
                .build();
    }

    /**
     * {@code CMD_SEND_CHANNEL_TXT_MSG} (0x03):
     * {@code 03 [txt_type] [channel_idx] [timestamp u32 LE] [text]}.
     *
     * <p>The app sends plaintext + channel index; the device performs
     * the over-the-air channel encryption.</p>
     *
     * @param channelIdx target channel slot
     * @param timestamp  sender timestamp (unix seconds)
     * @param text       message text (UTF-8)
     * @param txtType    text type (normally
     *                   {@link MeshcoreConstants#TXT_TYPE_PLAIN})
     * @return the encoded frame
     */
    public static byte[] sendChannelTextMessage(
            int channelIdx, long timestamp, String text, int txtType) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SEND_CHANNEL_MESSAGE.code())
                .u8(txtType)
                .u8(channelIdx)
                .u32(timestamp)
                .utf8String(text)
                .build();
    }

    /**
     * {@code CMD_SEND_CHANNEL_TXT_MSG} with
     * {@code txt_type = TXT_TYPE_PLAIN}.
     *
     * @param channelIdx target channel slot
     * @param timestamp  sender timestamp (unix seconds)
     * @param text       message text (UTF-8)
     * @return the encoded frame
     */
    public static byte[] sendChannelTextMessage(
            int channelIdx, long timestamp, String text) {
        return sendChannelTextMessage(channelIdx, timestamp, text,
                MeshcoreConstants.TXT_TYPE_PLAIN);
    }

    /**
     * {@code CMD_SEND_TXT_MSG} (0x02):
     * {@code 02 [txt_type] [attempt] [timestamp u32 LE]
     * [pubkey_prefix 6] [text]}.
     *
     * <p>The recipient is addressed by the first 6 bytes of its public
     * key (truncated/zero-padded to exactly 6).</p>
     *
     * @param pubKeyPrefix recipient public-key prefix
     * @param timestamp    sender timestamp (unix seconds)
     * @param text         message text (UTF-8)
     * @param txtType      text type
     * @param attempt      retry attempt counter
     * @return the encoded frame
     */
    public static byte[] sendTextMessage(
            byte[] pubKeyPrefix,
            long timestamp,
            String text,
            int txtType,
            int attempt) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SEND_TEXT_MESSAGE.code())
                .u8(txtType)
                .u8(attempt)
                .u32(timestamp)
                .fixed(pubKeyPrefix, MeshcoreConstants.PUB_KEY_PREFIX_SIZE)
                .utf8String(text)
                .build();
    }

    /**
     * {@code CMD_SEND_TXT_MSG} with plain text type and attempt 0.
     *
     * @param pubKeyPrefix recipient public-key prefix (6 bytes)
     * @param timestamp    sender timestamp (unix seconds)
     * @param text         message text (UTF-8)
     * @return the encoded frame
     */
    public static byte[] sendTextMessage(
            byte[] pubKeyPrefix, long timestamp, String text) {
        return sendTextMessage(pubKeyPrefix, timestamp, text,
                MeshcoreConstants.TXT_TYPE_PLAIN, 0);
    }

    /**
     * {@code CMD_SEND_SELF_ADVERT} (0x07):
     * {@code 07 [1=flood | 0=zero-hop]}.
     *
     * @param flood {@code true} = flood the advert, {@code false} =
     *              zero-hop only
     * @return the encoded frame
     */
    public static byte[] sendSelfAdvert(boolean flood) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SEND_SELF_ADVERT.code())
                .u8(flood ? 1 : 0)
                .build();
    }

    /**
     * {@code CMD_SET_ADVERT_NAME} (0x08):
     * {@code 08 [name UTF-8, ≤ 31 bytes]} (longer names are truncated
     * at the byte level).
     *
     * @param name advertised node name
     * @return the encoded frame
     */
    public static byte[] setAdvertName(String name) {
        byte[] n = name.getBytes(StandardCharsets.UTF_8);
        byte[] capped = n.length > MeshcoreConstants.MAX_ADVERT_NAME
                ? Arrays.copyOfRange(n, 0, MeshcoreConstants.MAX_ADVERT_NAME)
                : n;
        return new FrameBuilder()
                .u8(MeshcoreCommand.SET_ADVERT_NAME.code())
                .raw(capped)
                .build();
    }

    /**
     * {@code CMD_REMOVE_CONTACT} (0x0F): remove a contact by 32-byte
     * public key (frees a slot in the radio's contact table).
     *
     * @param publicKey the contact's full public key
     * @return the encoded frame
     */
    public static byte[] removeContact(byte[] publicKey) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.REMOVE_CONTACT.code())
                .fixed(publicKey, MeshcoreConstants.PUB_KEY_SIZE)
                .build();
    }

    /**
     * {@code CMD_ADD_UPDATE_CONTACT} (0x09): the 148-byte contact body
     * (same layout as {@code RESP_CODE_CONTACT}).
     *
     * @param c contact to add or replace
     * @return the encoded frame
     */
    public static byte[] addUpdateContact(Contact c) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.ADD_UPDATE_CONTACT.code())
                .fixed(c.publicKey(), MeshcoreConstants.PUB_KEY_SIZE)
                .u8(c.type())
                .u8(c.flags())
                .u8(c.outPathLen())
                .fixed(c.outPath(), MeshcoreConstants.MAX_PATH_SIZE)
                .fixed(c.name().getBytes(StandardCharsets.UTF_8),
                        MeshcoreConstants.CONTACT_NAME_SIZE)
                .u32(c.lastAdvertTimestamp())
                .i32(c.latitudeMicros())
                .i32(c.longitudeMicros())
                .u32(c.lastMod())
                .build();
    }

    // ---------------------------------------------------------------------
    // Device / radio configuration
    // ---------------------------------------------------------------------

    /**
     * {@code CMD_GET_CONTACT_BY_KEY} (0x1E): one contact by full public key.
     *
     * <p>The firmware replies with a single {@code RESP_CODE_CONTACT} frame or
     * {@code ERR_CODE_NOT_FOUND}. This is the terminal's connect path: a full
     * contact dump can exceed the command answer window and spends it on every
     * connect.</p>
     *
     * @param publicKey full 32-byte public key
     * @return the encoded frame
     * @throws IllegalArgumentException when the key is not 32 bytes
     */
    public static byte[] getContactByKey(byte[] publicKey) {
        if (publicKey.length != MeshcoreConstants.PUB_KEY_SIZE) {
            throw new IllegalArgumentException("public key must be exactly "
                    + MeshcoreConstants.PUB_KEY_SIZE + " bytes");
        }
        return new FrameBuilder()
                .u8(MeshcoreCommand.GET_CONTACT_BY_KEY.code())
                .fixed(publicKey, MeshcoreConstants.PUB_KEY_SIZE)
                .build();
    }

    /**
     * {@code CMD_GET_ADVERT_PATH} (0x2A): the route a recently heard advert took.
     *
     * <p>Body is {@code [reserved][32-byte public key]}. The firmware looks the
     * key up in its advert-path cache — matching on the first seven bytes, the
     * width of {@code AdvertPath::pubkey_prefix} — and answers
     * {@code RESP_CODE_ADVERT_PATH} (0x16), or {@code ERR_CODE_NOT_FOUND} when it
     * has not heard that node recently.</p>
     *
     * <p><strong>The whole key, not a prefix.</strong> The firmware's guard is
     * {@code len >= PUB_KEY_SIZE + 2} ({@code MyMesh.cpp:1851}), so a shorter frame
     * falls through to "unsupported command" and no route is ever learned.</p>
     *
     * @param publicKey the peer's full 32-byte public key
     * @return the encoded frame
     * @throws IllegalArgumentException when the key is not 32 bytes
     */
    public static byte[] getDefaultFloodScope() {
        return new FrameBuilder().u8(MeshcoreCommand.GET_DEFAULT_FLOOD_SCOPE.code()).build();
    }

    public static byte[] getAdvertPath(byte[] publicKey) {
        if (publicKey.length != MeshcoreConstants.PUB_KEY_SIZE) {
            throw new IllegalArgumentException("public key must be exactly "
                    + MeshcoreConstants.PUB_KEY_SIZE + " bytes");
        }
        return new FrameBuilder()
                .u8(MeshcoreCommand.GET_ADVERT_PATH.code())
                .u8(0) // reserved
                .fixed(publicKey, MeshcoreConstants.PUB_KEY_SIZE)
                .build();
    }

    /**
     * {@code CMD_RESET_PATH} (13): drop a contact's stored route.
     *
     * <p>The firmware matches on the full 32-byte public key and sets
     * {@code out_path_len = OUT_PATH_UNKNOWN} ({@code MyMesh.cpp:1277-1287}),
     * answering {@code OK} or {@code ERR_CODE_NOT_FOUND}. Sent when a direct
     * send comes back "not found": the route in the radio's contact record is
     * what a later contact read would hand back, so leaving it there would
     * relearn the path that just failed.</p>
     *
     * @param publicKey the peer's full 32-byte public key
     * @return the encoded frame
     * @throws IllegalArgumentException when the key is not 32 bytes
     */
    public static byte[] resetPath(byte[] publicKey) {
        if (publicKey.length != MeshcoreConstants.PUB_KEY_SIZE) {
            throw new IllegalArgumentException("public key must be exactly "
                    + MeshcoreConstants.PUB_KEY_SIZE + " bytes");
        }
        return new FrameBuilder()
                .u8(MeshcoreCommand.RESET_PATH.code())
                .fixed(publicKey, MeshcoreConstants.PUB_KEY_SIZE)
                .build();
    }

    /**
     * {@code CMD_SEND_RAW_DATA} (0x19) with an explicit encoded path length.
     *
     * <p>The firmware's {@code path_len} is the encoded byte used by
     * {@code Packet::writePath}: low six bits are the hop count and high two
     * bits are {@code hash_size - 1}. It is not the number of path bytes. For
     * a one-hop two-byte path the encoded value is {@code 0x41} and only two
     * path bytes follow. The frame budget is still
     * {@code 176 - 2 - path.length}.</p>
     *
     * <p>Firmware 1.17.1/1.18 checks {@code path_len} as a byte count in
     * {@code CMD_SEND_RAW_DATA}, so a two- or three-byte hash path is rejected
     * there. Use {@link #sendRawPacket(int, byte[])} above {@code 0x3F}.</p>
     *
     * @param pathLen encoded path length (0..0x7F)
     * @param path    actual path bytes ({@code hops * hash_size})
     * @param payload raw-custom bytes (opaque to MeshCore, at least 4)
     * @return the encoded frame
     * @throws IllegalArgumentException when the encoding, path or payload is
     *                                  outside the companion frame
     */
    public static byte[] sendRawData(int pathLen, byte[] path, byte[] payload) {
        if (pathLen < 0 || pathLen > 0x7F) {
            throw new IllegalArgumentException(
                    "raw path length encoding must be 0..127, got " + pathLen);
        }
        int hashSize = (pathLen >>> 6) + 1;
        int hops = pathLen & 0x3F;
        int expected = hops * hashSize;
        if (expected > MeshcoreConstants.MAX_PATH_SIZE) {
            throw new IllegalArgumentException(
                    "raw path needs " + expected + " bytes, maximum "
                            + MeshcoreConstants.MAX_PATH_SIZE);
        }
        if (path.length != expected) {
            throw new IllegalArgumentException(
                    "raw path encoding " + pathLen + " means " + expected
                            + " path bytes, got " + path.length);
        }
        if (payload.length < MeshcoreConstants.MIN_RAW_PAYLOAD) {
            throw new IllegalArgumentException(
                    "raw payload must be at least "
                            + MeshcoreConstants.MIN_RAW_PAYLOAD + " bytes");
        }
        int maxPayload = MeshcoreConstants.MAX_RAW_PAYLOAD - path.length;
        if (payload.length > maxPayload) {
            throw new IllegalArgumentException(
                    "raw payload is " + payload.length + " bytes with a "
                            + path.length + "-byte path, maximum " + maxPayload);
        }
        return new FrameBuilder()
                .u8(MeshcoreCommand.SEND_RAW_DATA.code())
                .u8(pathLen)
                .raw(path)
                .raw(payload)
                .build();
    }

    /**
     * {@code CMD_SEND_RAW_PACKET} (0x41): {@code 41 [priority][packet]}.
     *
     * <p>Use this when the path encoding cannot be expressed by
     * {@code CMD_SEND_RAW_DATA}'s byte-count bounds check, or when the caller
     * needs a route other than direct. The packet is serialised in full,
     * including its header and encoded path.</p>
     *
     * @param priority transmit priority
     * @param packet   complete serialised packet
     * @return the encoded companion frame
     * @throws IllegalArgumentException when the packet is empty or exceeds
     *                                  the companion frame
     */
    public static byte[] sendRawPacket(int priority, byte[] packet) {
        if (priority < 0 || priority > 0xFF) {
            throw new IllegalArgumentException("priority must fit one byte");
        }
        if (packet.length == 0) {
            throw new IllegalArgumentException("raw packet must not be empty");
        }
        if (packet.length > MeshcoreConstants.MAX_RAW_PACKET) {
            throw new IllegalArgumentException(
                    "raw packet is " + packet.length + " bytes, maximum "
                            + MeshcoreConstants.MAX_RAW_PACKET);
        }
        return new FrameBuilder()
                .u8(MeshcoreCommand.SEND_RAW_PACKET.code())
                .u8(priority)
                .raw(packet)
                .build();
    }

    /**
     * {@code CMD_SET_PATH_HASH_MODE} (0x3D): {@code 3D 00 [mode]}.
     *
     * @param mode path-hash mode (0..2)
     * @return the encoded frame
     */
    public static byte[] setPathHashMode(int mode) {
        if (mode < 0 || mode > 2) {
            throw new IllegalArgumentException(
                    "path hash mode must be 0..2, got " + mode);
        }
        return new FrameBuilder()
                .u8(MeshcoreCommand.SET_PATH_HASH_MODE.code())
                .u8(0)
                .u8(mode)
                .build();
    }

    /**
     * {@code CMD_SET_RADIO_PARAMS} (0x0B):
     * {@code 0B [freq u32 LE ×1000] [bw u32 LE ×1000] [sf] [cr]
     * [repeat?]}.
     *
     * @param p radio parameters; the optional repeat byte is appended
     *          only when present
     * @return the encoded frame
     */
    public static byte[] setRadioParams(RadioParams p) {
        FrameBuilder b = new FrameBuilder()
                .u8(MeshcoreCommand.SET_RADIO_PARAMS.code())
                .u32(Math.round(p.frequencyMhz() * MeshcoreConstants.RADIO_SCALE))
                .u32(Math.round(p.bandwidthKhz() * MeshcoreConstants.RADIO_SCALE))
                .u8(p.spreadingFactor())
                .u8(p.codingRate());
        if (p.repeat().isPresent()) {
            b.u8(p.repeat().getAsInt());
        }
        return b.build();
    }

    /**
     * {@code CMD_SET_RADIO_TX_POWER} (0x0C): {@code 0C [power int8 dBm]}.
     *
     * @param dbm TX power in dBm (signed byte semantics)
     * @return the encoded frame
     */
    public static byte[] setRadioTxPower(int dbm) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SET_RADIO_TX_POWER.code())
                .u8(dbm & 0xFF)
                .build();
    }

    /**
     * {@code CMD_SET_ADVERT_LATLON} (0x0E):
     * {@code 0E [lat i32 LE ×1e6] [lon i32 LE ×1e6]}.
     *
     * @param latitudeMicros  latitude in signed micro-degrees
     * @param longitudeMicros longitude in signed micro-degrees
     * @return the encoded frame
     */
    public static byte[] setAdvertLatLon(
            int latitudeMicros, int longitudeMicros) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SET_ADVERT_LATLON.code())
                .i32(latitudeMicros)
                .i32(longitudeMicros)
                .build();
    }

    /**
     * {@code CMD_SET_ADVERT_LATLON} (0x0E) with the optional altitude:
     * {@code 0E [lat i32 LE ×1e6] [lon i32 LE ×1e6] [alt i32 LE]}.
     *
     * @param latitudeMicros  latitude in signed micro-degrees
     * @param longitudeMicros longitude in signed micro-degrees
     * @param altitudeMicros  altitude (micro-scaled, firmware-defined)
     * @return the encoded frame
     */
    public static byte[] setAdvertLatLon(
            int latitudeMicros, int longitudeMicros, int altitudeMicros) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SET_ADVERT_LATLON.code())
                .i32(latitudeMicros)
                .i32(longitudeMicros)
                .i32(altitudeMicros)
                .build();
    }

    /**
     * {@code CMD_SET_OTHER_PARAMS} (0x26):
     * {@code 26 [manual_add] [telemetry_packed] [loc_policy?]
     * [multi_acks?]}.
     *
     * <p>{@code advertLocPolicy} and {@code multiAcks} are optional
     * trailing bytes; {@code multiAcks} can only be sent when
     * {@code advertLocPolicy} is also present (wire order).</p>
     *
     * @param manualAddContacts   manual-add policy byte
     * @param telemetryModePacked packed per-class telemetry mode byte
     * @param advertLocPolicy     optional advert location policy
     * @param multiAcks           optional multi-ACK setting (requires
     *                            {@code advertLocPolicy})
     * @return the encoded frame
     */
    public static byte[] setOtherParams(
            int manualAddContacts,
            int telemetryModePacked,
            Integer advertLocPolicy,
            Integer multiAcks) {
        FrameBuilder b = new FrameBuilder()
                .u8(MeshcoreCommand.SET_OTHER_PARAMS.code())
                .u8(manualAddContacts)
                .u8(telemetryModePacked);
        if (advertLocPolicy != null) {
            b.u8(advertLocPolicy);
            if (multiAcks != null) {
                b.u8(multiAcks);
            }
        }
        return b.build();
    }

    /**
     * {@code CMD_GET_CUSTOM_VARS} (0x28): request the device's custom
     * string-keyed settings ({@code gps}, {@code gps_interval}, etc.).
     * Reply is {@code RESP_CODE_CUSTOM_VARS} (0x15) decoded into a
     * {@link CustomVarsFrame}.
     *
     * @return the encoded frame
     */
    public static byte[] getCustomVars() {
        return new FrameBuilder()
                .u8(MeshcoreCommand.GET_CUSTOM_VARS.code())
                .build();
    }

    /**
     * {@code CMD_SET_CUSTOM_VAR} (0x29): write a single string-keyed
     * setting. Body: {@code [opcode][name]:[value]} (ASCII, no NUL —
     * frame length defines the end).
     *
     * <p>The firmware splits on the first {@code :}, so the name must
     * not contain one; values containing {@code ,} are fine since the
     * comma is not a separator on the request path.</p>
     *
     * @param name  variable name (must not contain {@code :})
     * @param value variable value
     * @return the encoded frame
     * @throws IllegalArgumentException if {@code name} contains {@code :}
     */
    public static byte[] setCustomVar(String name, String value) {
        if (name.contains(":")) {
            throw new IllegalArgumentException(
                    "custom var name must not contain \":\" (frame separator)");
        }
        return new FrameBuilder()
                .u8(MeshcoreCommand.SET_CUSTOM_VAR.code())
                .utf8String(name + ":" + value)
                .build();
    }

    /**
     * {@code CMD_SEND_TELEMETRY_REQ} (0x27) for <em>self</em> telemetry:
     * {@code 27 [0 0 0]}. The device replies immediately (no OTA) with
     * {@code PUSH_CODE_TELEMETRY_RESPONSE} (0x8B) carrying its own
     * CayenneLPP payload.
     *
     * @return the encoded frame
     */
    public static byte[] sendTelemetryReq() {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SEND_TELEMETRY_REQ.code())
                .zeros(3)
                .build();
    }

    /**
     * {@code CMD_SEND_TELEMETRY_REQ} (0x27) for a <em>peer</em>:
     * {@code 27 [0 0 0] [pub_key 32B]}. The device queries the peer
     * over the air; the same 0x8B push arrives later (seconds) if the
     * peer responds. The caller is responsible for batching/back-off.
     *
     * <p>The 3 bytes between opcode and pubkey are reserved (zero in
     * {@code companion-v1.15.0}); sending zeros is safe today.</p>
     *
     * @param peerPubKey the peer's full 32-byte public key
     * @return the encoded frame
     * @throws IllegalArgumentException if the key is not 32 bytes
     */
    public static byte[] sendTelemetryReq(byte[] peerPubKey) {
        if (peerPubKey.length != MeshcoreConstants.PUB_KEY_SIZE) {
            throw new IllegalArgumentException("peerPubKey must be exactly "
                    + MeshcoreConstants.PUB_KEY_SIZE + " bytes");
        }
        return new FrameBuilder()
                .u8(MeshcoreCommand.SEND_TELEMETRY_REQ.code())
                .zeros(3)
                .fixed(peerPubKey, MeshcoreConstants.PUB_KEY_SIZE)
                .build();
    }

    /**
     * {@code CMD_SET_TUNING_PARAMS} (0x15):
     * {@code 15 [rx_delay_base u32 LE ×1000] [airtime_factor u32 LE
     * ×1000]}.
     *
     * @param rxDelayBaseSeconds receive-delay base in seconds
     * @param airtimeFactor      airtime factor
     * @return the encoded frame
     */
    public static byte[] setTuningParams(
            double rxDelayBaseSeconds, double airtimeFactor) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SET_TUNING_PARAMS.code())
                .u32(Math.round(rxDelayBaseSeconds * MeshcoreConstants.RADIO_SCALE))
                .u32(Math.round(airtimeFactor * MeshcoreConstants.RADIO_SCALE))
                .build();
    }

    /**
     * {@code CMD_DEVICE_QUERY} (0x16): {@code 16 [app_target_ver]}.
     *
     * @param appTargetVer protocol version the app targets (normally 1)
     * @return the encoded frame
     */
    public static byte[] deviceQuery(int appTargetVer) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.DEVICE_QUERY.code())
                .u8(appTargetVer)
                .build();
    }

    /**
     * {@code CMD_GET_BATT_AND_STORAGE} (0x14): {@code 14}.
     *
     * @return the encoded frame
     */
    public static byte[] getBatteryStorage() {
        return new FrameBuilder()
                .u8(MeshcoreCommand.GET_BATTERY_STORAGE.code())
                .build();
    }

    /**
     * {@code CMD_GET_CHANNEL} (0x1F): {@code 1F [channel_idx]}.
     *
     * @param channelIdx channel slot index
     * @return the encoded frame
     */
    public static byte[] getChannel(int channelIdx) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.GET_CHANNEL.code())
                .u8(channelIdx)
                .build();
    }

    /**
     * {@code CMD_SET_CHANNEL} (0x20):
     * {@code 20 [channel_idx] [name 32B NUL-padded] [secret 16B]}
     * (50 bytes).
     *
     * @param channelIdx channel slot index
     * @param name       channel name (zero-padded/truncated to 32 bytes)
     * @param psk        the 16-byte AES-128 channel key as carried by
     *                   the companion link (truncated/zero-padded to
     *                   {@link MeshcoreConstants#CHANNEL_PSK_SIZE})
     * @return the encoded frame
     */
    public static byte[] setChannel(int channelIdx, String name, byte[] psk) {
        return new FrameBuilder()
                .u8(MeshcoreCommand.SET_CHANNEL.code())
                .u8(channelIdx)
                .fixed(name.getBytes(StandardCharsets.UTF_8),
                        MeshcoreConstants.CHANNEL_NAME_SIZE)
                .fixed(psk, MeshcoreConstants.CHANNEL_PSK_SIZE)
                .build();
    }

    // ---------------------------------------------------------------------
    // Decoder (Device → App) — total, never throws.
    // ---------------------------------------------------------------------

    /**
     * Decodes one device&#8594;app frame.
     *
     * <p>Total: returns a {@link DecodeFailure} for malformed/truncated
     * input and an {@link UnsupportedFrame} for unmodelled opcodes —
     * never throws. Switch exhaustively over
     * {@link MeshcoreInbound}.</p>
     *
     * @param frame raw frame bytes (one BLE notification)
     * @return the decoded inbound value
     */
    public static MeshcoreInbound decode(byte[] frame) {
        if (frame.length == 0) {
            return new DecodeFailure(MeshcoreDecodeError.of(
                    DecodeErrorKind.EMPTY, "empty frame"));
        }
        int op = frame[0] & 0xFF;
        ByteCursor c = new ByteCursor(frame);
        try {
            c.u8("opcode"); // consume opcode
            switch (op) {
                case 0x00: { // RESP_CODE_OK
                    OptionalLong v = c.remaining() >= 4
                            ? OptionalLong.of(c.u32("ok.value"))
                            : OptionalLong.empty();
                    return new OkFrame(v);
                }
                case 0x01: { // RESP_CODE_ERR
                    OptionalInt code = c.remaining() >= 1
                            ? OptionalInt.of(c.u8("err.code"))
                            : OptionalInt.empty();
                    return new ErrorFrame(code);
                }
                case 0x02: // RESP_CODE_CONTACTS_START
                    return new ContactsStartFrame(c.u32("contactsStart.count"));

                case 0x03: // RESP_CODE_CONTACT
                    return new ContactFrame(decodeContact(c));

                case 0x04: // RESP_CODE_END_OF_CONTACTS
                    return new EndOfContactsFrame(c.u32("endOfContacts.lastMod"));

                case 0x05: // RESP_CODE_SELF_INFO
                    return new SelfInfoFrame(decodeSelfInfo(c));

                case 0x06: // RESP_CODE_SENT
                    return new MsgSentFrame(new MsgSent(
                            c.u8("msgSent.floodFlag") != 0,
                            c.u32("msgSent.expectedAck"),
                            c.u32("msgSent.estTimeoutMs")));

                case 0x07: // RESP_CODE_CONTACT_MSG_RECV (legacy)
                    return new ContactMessageFrame(decodeContactMsg(c, false));

                case 0x08: // RESP_CODE_CHANNEL_MSG_RECV (legacy)
                    return new ChannelMessageFrame(decodeChannelMsg(c, false));

                case 0x09: // RESP_CODE_CURR_TIME
                    return new CurrentTimeFrame(c.u32("currentTime.unix"));

                case 0x0A: // RESP_CODE_NO_MORE_MESSAGES
                    return new NoMoreMessagesFrame();

                case 0x0C: // RESP_CODE_BATT_AND_STORAGE
                    return new BatteryStorageFrame(new BatteryStorage(
                            c.u16("battery.mv"),
                            c.u32("battery.usedKb"),
                            c.u32("battery.totalKb")));

                case 0x0D: // RESP_CODE_DEVICE_INFO
                    return new DeviceInfoFrame(new DeviceInfo(
                            c.u8("deviceInfo.fwVer"),
                            c.u8("deviceInfo.maxContactsHalf") * 2,
                            c.u8("deviceInfo.maxGroupChannels"),
                            c.u32("deviceInfo.blePin"),
                            c.fixedCString(
                                    MeshcoreConstants.DEVICE_BUILD_DATE_SIZE,
                                    "deviceInfo.build"),
                            c.fixedCString(
                                    MeshcoreConstants.DEVICE_MANUFACTURER_SIZE,
                                    "deviceInfo.manufacturer"),
                            c.fixedCString(
                                    MeshcoreConstants.DEVICE_FIRMWARE_VERSION_SIZE,
                                    "deviceInfo.fwVersion"),
                            c.u8("deviceInfo.clientRepeat"),
                            c.u8("deviceInfo.pathHashMode")));

                case 0x10: // RESP_CODE_CONTACT_MSG_RECV_V3
                    return new ContactMessageFrame(decodeContactMsg(c, true));

                case 0x11: // RESP_CODE_CHANNEL_MSG_RECV_V3
                    return new ChannelMessageFrame(decodeChannelMsg(c, true));

                case 0x12: // RESP_CODE_CHANNEL_INFO
                    return new ChannelInfoFrame(new ChannelInfo(
                            c.u8("channelInfo.idx"),
                            c.fixedCString(MeshcoreConstants.CHANNEL_NAME_SIZE,
                                    "channelInfo.name"),
                            c.bytes(MeshcoreConstants.CHANNEL_PSK_SIZE,
                                    "channelInfo.psk")));

                case 0x1C: { // RESP_CODE_DEFAULT_FLOOD_SCOPE
                    // The code alone is the firmware's "this radio has no default scope" answer.
                    if (c.atEnd()) {
                        return new DefaultFloodScopeFrame("", null);
                    }
                    // [name 31][key 16]: a name longer than the field is impossible, and a frame
                    // shorter than the pair is a truncated answer rather than a scope.
                    if (c.remaining() < 31 + 16) {
                        return new DecodeFailure(MeshcoreDecodeError.of(
                                DecodeErrorKind.TRUNCATED,
                                "defaultFloodScope needs 47 bytes after the code, has "
                                        + c.remaining(),
                                0x1C));
                    }
                    String scopeName = new String(
                            c.bytes(31, "defaultFloodScope.name"),
                            java.nio.charset.StandardCharsets.UTF_8);
                    int nul = scopeName.indexOf('\0');
                    if (nul >= 0) {
                        scopeName = scopeName.substring(0, nul);
                    }
                    byte[] scopeKey = c.bytes(16, "defaultFloodScope.key");
                    return new DefaultFloodScopeFrame(scopeName.trim(), scopeKey);
                }
                case 0x16: { // RESP_CODE_ADVERT_PATH
                    int receivedAt = (int) c.u32("advertPath.receivedAt");
                    int pathLen = c.u8("advertPath.pathLen");
                    int hashSize = ((pathLen >> 6) & 0x03) + 1;
                    if (hashSize == 4) { // reserved by the firmware, so the frame is malformed
                        return new DecodeFailure(MeshcoreDecodeError.of(
                                DecodeErrorKind.MALFORMED,
                                "advertPath claims the reserved 4-byte hash width",
                                0x16));
                    }
                    int want = (pathLen & 0x3F) * hashSize;
                    if (c.remaining() < want) {
                        return new DecodeFailure(MeshcoreDecodeError.of(
                                DecodeErrorKind.TRUNCATED,
                                "advertPath needs " + want + " bytes, has " + c.remaining(),
                                0x16));
                    }
                    byte[] path = want == 0
                            ? new byte[0]
                            : c.bytes(want, "advertPath.path");
                    return new AdvertPathFrame(receivedAt, pathLen, path);
                }
                case 0x15: // RESP_CODE_CUSTOM_VARS
                    return new CustomVarsFrame(decodeCustomVars(c));

                case 0x80: { // PUSH_CODE_ADVERTISEMENT
                    byte[] payload = c.atEnd()
                            ? new byte[0]
                            : c.bytes(c.remaining(), "advert.payload");
                    // Firmware in the field pushes only the 32-byte public key
                    // here, not a signed advert record. Parsing that as a
                    // record fails on the timestamp and drops the push, so
                    // every over-the-air advert is lost. Short payloads decode
                    // to a BARE advert instead: which node was heard, and
                    // nothing it cannot honestly claim.
                    if (payload.length >= Advert.MIN_FULL_SIZE) {
                        return new AdvertFrame(Advert.parse(payload));
                    }
                    if (payload.length >= MeshcoreConstants.PUB_KEY_SIZE) {
                        return new AdvertFrame(Advert.bare(
                                java.util.Arrays.copyOf(
                                        payload, MeshcoreConstants.PUB_KEY_SIZE)));
                    }
                    return new AdvertFrame(Advert.bare(payload));
                }
                case 0x82: { // PUSH_CODE_ACK
                    // Delivery ACK for a direct message: a 4-byte CRC tag
                    // matching the MsgSent.expectedAck recorded on send.
                    // Some firmware appends round-trip info; we only need
                    // the leading tag. Guard for a short/empty payload.
                    long ack = c.remaining() >= 4 ? c.u32("ack.crc") : 0;
                    return new AckFrame(ack);
                }
                case 0x83: { // PUSH_CODE_MSGS_WAITING
                    OptionalInt n = c.remaining() >= 1
                            ? OptionalInt.of(c.u8("msgsWaiting.count"))
                            : OptionalInt.empty();
                    return new MessagesWaitingFrame(n);
                }
                case 0x84: { // PUSH_CODE_RAW_DATA
                    double snr = c.i8("rawData.snr") / 4.0;
                    int rssi = c.i8("rawData.rssi");
                    if (c.atEnd() || c.u8("rawData.marker")
                            != MeshcoreConstants.RAW_DATA_MARKER) {
                        return new DecodeFailure(MeshcoreDecodeError.of(
                                DecodeErrorKind.MALFORMED,
                                "rawData marker is not 0xFF", 0x84));
                    }
                    if (c.atEnd()) {
                        return new DecodeFailure(MeshcoreDecodeError.of(
                                DecodeErrorKind.MALFORMED,
                                "rawData payload is empty", 0x84));
                    }
                    byte[] payload = c.bytes(c.remaining(), "rawData.payload");
                    return new RawDataFrame(snr, rssi, payload);
                }
                case 0x88: { // PUSH_CODE_LOG_RX_DATA
                    double snr = c.i8("rfLog.snr") / 4.0;
                    int rssi = c.i8("rfLog.rssi");
                    byte[] raw = c.atEnd()
                            ? new byte[0]
                            : c.bytes(c.remaining(), "rfLog.raw");
                    return new RfLogFrame(new RfLog(snr, rssi, raw));
                }
                case 0x8B: { // PUSH_CODE_TELEMETRY_RESPONSE
                    c.u8("telemetry.reserved");
                    byte[] pub6 = c.bytes(
                            MeshcoreConstants.PUB_KEY_PREFIX_SIZE,
                            "telemetry.pubKey6");
                    byte[] lpp = c.atEnd()
                            ? new byte[0]
                            : c.bytes(c.remaining(), "telemetry.lpp");
                    return new TelemetryResponseFrame(pub6, lpp);
                }
                default:
                    return new UnsupportedFrame(op, frame.clone());
            }
        } catch (FrameTruncatedException e) {
            return new DecodeFailure(MeshcoreDecodeError.of(
                    DecodeErrorKind.TRUNCATED, e.getMessage(), op));
        }
    }

    /**
     * {@code RESP_CODE_CUSTOM_VARS} payload: comma-separated
     * {@code name:value} pairs (ASCII, no NUL — frame length is the
     * terminator). Empty payload &#8594; empty map. Malformed entries
     * (no {@code :} separator) are silently dropped to match the
     * firmware's permissive parser.
     */
    private static Map<String, String> decodeCustomVars(ByteCursor c) {
        if (c.atEnd()) {
            return Collections.emptyMap();
        }
        String payload = c.utf8ToEnd("customVars.payload");
        Map<String, String> out = new LinkedHashMap<>();
        for (String entry : payload.split(",", -1)) {
            int sep = entry.indexOf(':');
            if (sep <= 0) {
                continue;
            }
            out.put(entry.substring(0, sep), entry.substring(sep + 1));
        }
        return Collections.unmodifiableMap(out);
    }

    private static SelfInfo decodeSelfInfo(ByteCursor c) {
        int advType = c.u8("selfInfo.advType");
        int txPower = c.u8("selfInfo.txPower");
        int maxTxPower = c.u8("selfInfo.maxTxPower");
        byte[] pubKey = c.bytes(MeshcoreConstants.PUB_KEY_SIZE,
                "selfInfo.pubKey");
        double lat = c.i32("selfInfo.lat") / 1e6;
        double lon = c.i32("selfInfo.lon") / 1e6;
        int multiAcks = c.u8("selfInfo.multiAcks");
        int locPolicy = c.u8("selfInfo.advertLocPolicy");
        int telemetry = c.u8("selfInfo.telemetryMode");
        boolean manualAdd = c.u8("selfInfo.manualAddContacts") != 0;
        double freq = c.u32("selfInfo.frequency") / 1000.0;
        double bw = c.u32("selfInfo.bandwidth") / 1000.0;
        int sf = c.u8("selfInfo.sf");
        int cr = c.u8("selfInfo.cr");
        String name = c.atEnd() ? "" : c.utf8ToEnd("selfInfo.name");
        return new SelfInfo(advType, txPower, maxTxPower, pubKey, lat, lon,
                multiAcks, locPolicy, telemetry, manualAdd, freq, bw, sf, cr,
                name);
    }

    /**
     * Decodes 0x08 (legacy) and 0x11 (V3) channel messages. The opcode
     * byte has already been consumed.
     */
    private static ChannelMessage decodeChannelMsg(ByteCursor c, boolean v3) {
        Double snr = null;
        if (v3) {
            snr = c.i8("channelMsg.snr") / 4.0;
            c.u8("channelMsg.reserved1");
            c.u8("channelMsg.reserved2");
        }
        int channelIdx = c.u8("channelMsg.channelIdx");
        int pathLen = c.u8("channelMsg.pathLen");
        int txtType = c.u8("channelMsg.txtType");
        long ts = c.u32("channelMsg.timestamp");
        String text = c.atEnd() ? "" : c.utf8ToEnd("channelMsg.text");
        return new ChannelMessage(channelIdx, pathLen, txtType, ts, text, v3,
                snr);
    }

    /**
     * Decodes 0x07 (legacy) and 0x10 (V3) contact messages. The opcode
     * byte has already been consumed.
     */
    private static ContactMessage decodeContactMsg(ByteCursor c, boolean v3) {
        Double snr = null;
        if (v3) {
            snr = c.i8("contactMsg.snr") / 4.0;
            c.u8("contactMsg.reserved1");
            c.u8("contactMsg.reserved2");
        }
        byte[] prefix = c.bytes(MeshcoreConstants.PUB_KEY_PREFIX_SIZE,
                "contactMsg.prefix");
        int pathLen = c.u8("contactMsg.pathLen");
        int txtType = c.u8("contactMsg.txtType");
        long ts = c.u32("contactMsg.timestamp");
        byte[] sig = null;
        if (txtType == MeshcoreConstants.TXT_TYPE_SIGNED_PLAIN) {
            sig = c.bytes(MeshcoreConstants.SIGNATURE_PREFIX_SIZE,
                    "contactMsg.sigPrefix");
        }
        String text = c.atEnd() ? "" : c.utf8ToEnd("contactMsg.text");
        return new ContactMessage(prefix, pathLen, txtType, ts, text, v3, snr,
                sig);
    }

    private static Contact decodeContact(ByteCursor c) {
        byte[] pubKey = c.bytes(MeshcoreConstants.PUB_KEY_SIZE,
                "contact.pubKey");
        int type = c.u8("contact.type");
        int flags = c.u8("contact.flags");
        int pathLen = c.u8("contact.outPathLen");
        byte[] path = c.bytes(MeshcoreConstants.MAX_PATH_SIZE,
                "contact.outPath");
        String name = c.fixedCString(MeshcoreConstants.CONTACT_NAME_SIZE,
                "contact.name");
        long advertTs = c.u32("contact.lastAdvertTimestamp");
        int latMicros = c.i32("contact.lat");
        int lonMicros = c.i32("contact.lon");
        long lastMod = c.u32("contact.lastMod");
        return new Contact(pubKey, type, flags, pathLen, path, name, advertTs,
                latMicros, lonMicros, lastMod);
    }
}
