// Copyright (c) 2026 IoTone, Inc.
// SPDX-License-Identifier: MIT
package io.iotone.meshcore.android;

import androidx.annotation.NonNull;
import androidx.annotation.Nullable;

import java.io.IOException;
import java.util.ArrayDeque;
import java.util.Deque;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

import io.iotone.meshcore.codec.MeshcoreFrameCodec;
import io.iotone.meshcore.frames.AdvertFrame;
import io.iotone.meshcore.frames.ChannelMessageFrame;
import io.iotone.meshcore.frames.ContactFrame;
import io.iotone.meshcore.frames.ContactMessageFrame;
import io.iotone.meshcore.frames.DecodeFailure;
import io.iotone.meshcore.frames.MeshcoreInbound;
import io.iotone.meshcore.frames.MessagesWaitingFrame;
import io.iotone.meshcore.frames.NoMoreMessagesFrame;
import io.iotone.meshcore.frames.OkFrame;
import io.iotone.meshcore.frames.ErrorFrame;
import io.iotone.meshcore.frames.RawDataFrame;
import io.iotone.meshcore.frames.SelfInfoFrame;
import io.iotone.meshcore.model.RadioParams;
import io.iotone.meshcore.model.SelfInfo;
import io.iotone.meshcore.transport.MeshcoreTransport;

/**
 * MeshCore protocol session over any {@link MeshcoreTransport}.
 *
 * <p>Drives the connection lifecycle end-to-end:</p>
 * <ol>
 *   <li><strong>Handshake</strong> — on transport ready, sends
 *       {@code CMD_APP_START} and waits for {@code RESP_CODE_SELF_INFO}.
 *       Transitions to {@link SessionState#READY} and calls
 *       {@link SessionListener#onReady}.</li>
 *   <li><strong>Message pump</strong> — when {@code PUSH_CODE_MSGS_WAITING}
 *       (0x83) arrives, the session drains the device's queue by issuing
 *       {@code CMD_SYNC_NEXT_MESSAGE} (0x0A) repeatedly until
 *       {@code RESP_CODE_NO_MORE_MESSAGES} (0x0A) is received.</li>
 *   <li><strong>Frame dispatch</strong> — decoded frames are routed to
 *       the typed {@link SessionListener} callbacks on the main thread
 *       (inherited from the transport's dispatch thread).</li>
 * </ol>
 *
 * <p>RF-log frames (0x88) are buffered internally; use
 * {@link #exportInteropFixture} to produce a fixture JSON for the
 * {@code libmeshcore} interop test suite (see M6 runbook).</p>
 *
 * <h3>Usage</h3>
 * <pre>{@code
 * AndroidBleTransport transport = new AndroidBleTransport(context);
 * MeshcoreSession session = new MeshcoreSession("Meshmore", transport, listener);
 * transport.connect(device);   // triggers the handshake automatically
 * // …later…
 * session.sendChannelMessage(0, System.currentTimeMillis() / 1000, "hello");
 * }</pre>
 *
 * <p>All listener callbacks and all internal state transitions run on
 * the same thread the transport dispatches on (Android main thread for
 * {@link AndroidBleTransport}).</p>
 */
public final class MeshcoreSession {

    private static final int RF_LOG_BUFFER_CAP = 32;

    private final String appName;
    private final MeshcoreTransport transport;
    private final SessionListener listener;

    private final AtomicReference<SessionState> state =
            new AtomicReference<>(SessionState.DISCONNECTED);

    /** Most recent SELF_INFO received during the handshake. */
    @Nullable
    private volatile SelfInfo selfInfo;

    /**
     * Ring buffer of the last {@link #RF_LOG_BUFFER_CAP} raw inbound frames.
     * Bytes are kept, not hex strings: hex is only needed when a fixture is
     * exported, and formatting every raw terminal record on the BLE callback
     * thread was measurable CPU and allocation cost.
     */
    private final Deque<byte[]> rfLogCapture =
            new ArrayDeque<>(RF_LOG_BUFFER_CAP);

    /** True while the sync-drain loop is in progress. */
    private volatile boolean draining = false;

    /**
     * Whether the session drains the firmware's message queue.
     *
     * <p>LDMUX carries only raw custom records and has no consumer for
     * MeshCore channel/DM messages. Leaving the pump on adds a
     * {@code SYNC_NEXT_MESSAGE} command for every queued item, which competes
     * with the one uncorrelated raw OK/ERR window. It is off for LDMUX.</p>
     */
    private volatile boolean autoDrain = true;

    /**
     * The one raw send awaiting a command answer, or null.
     *
     * <p>The companion protocol has no command sequence number: OK/ERR
     * belongs to the most recent command. LDMUX's adapter therefore keeps at
     * most one raw send in flight, and a timeout is a dead link rather than a
     * reason to start another command that a late answer could resolve.</p>
     */
    private final AtomicReference<RawSendCallback> pendingRaw =
            new AtomicReference<>();

    /**
     * Number of tracked commands queued or in flight.
     *
     * <p>Tracked commands are the ones LDMUX coexists with: radio config,
     * contacts and device query. Raw sends are refused while one is
     * outstanding. The companion protocol has no sequence number, so this is
     * the only way to keep a config OK from resolving a raw callback.</p>
     *
     * <p>Only one tracked command is written at a time. The queue is the
     * firmware's single answer slot made explicit: three preset writes must
     * not race three OK/ERR frames that carry no command id.</p>
     */
    private final Object trackedLock = new Object();
    private final java.util.ArrayDeque<byte[]> trackedQueue =
            new java.util.ArrayDeque<>();
    private int pendingTracked;
    private boolean trackedInFlight;

    /**
     * Creates a session.
     *
     * @param appName   the app name announced to the device in
     *                  {@code CMD_APP_START} (max 64 UTF-8 bytes; no NUL)
     * @param transport the BLE (or other) transport to use; the session
     *                  registers itself as the listener
     * @param listener  receives typed frame callbacks and state changes
     */
    public MeshcoreSession(
            @NonNull String appName,
            @NonNull MeshcoreTransport transport,
            @NonNull SessionListener listener) {
        this.appName = appName;
        this.transport = transport;
        this.listener = listener;
        transport.setListener(new TransportListener());
    }

    // -------------------------------------------------------------------------
    // Public API
    // -------------------------------------------------------------------------

    /**
     * Returns the current session state.
     *
     * @return session state, never {@code null}
     */
    @NonNull
    public SessionState state() {
        return state.get();
    }

    /**
     * Returns the device self-info received during the handshake, or
     * {@code null} before the handshake completes.
     *
     * @return decoded {@link SelfInfo}, or {@code null}
     */
    @Nullable
    public SelfInfo selfInfo() {
        return selfInfo;
    }

    /**
     * Sends a channel text message. No-op when the session is not
     * {@link SessionState#READY}.
     *
     * @param channelIdx target channel slot (0-based)
     * @param timestamp  sender timestamp (unix seconds)
     * @param text       message text (UTF-8)
     */
    public void sendChannelMessage(int channelIdx, long timestamp,
            @NonNull String text) {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.sendChannelTextMessage(
                channelIdx, timestamp, text));
    }

    /**
     * Sends a direct message to a contact. No-op when not
     * {@link SessionState#READY}.
     *
     * @param pubKeyPrefix first 6 bytes of the recipient's public key
     * @param timestamp    sender timestamp (unix seconds)
     * @param text         message text (UTF-8)
     */
    public void sendDirectMessage(
            @NonNull byte[] pubKeyPrefix, long timestamp,
            @NonNull String text) {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.sendTextMessage(pubKeyPrefix, timestamp, text));
    }

    /**
     * Emits a self-advert into the mesh. No-op when not
     * {@link SessionState#READY}.
     *
     * @param flood {@code true} = flood, {@code false} = zero-hop
     */
    public void sendSelfAdvert(boolean flood) {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.sendSelfAdvert(flood));
    }

    /**
     * Requests self-telemetry from the device. The response arrives as
     * a {@code PUSH_CODE_TELEMETRY_RESPONSE} (0x8B) frame dispatched
     * to {@link SessionListener#onOtherFrame}. No-op when not
     * {@link SessionState#READY}.
     */
    public void requestSelfTelemetry() {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.sendTelemetryReq());
    }

    /**
     * Requests telemetry from a PEER ({@code CMD_SEND_TELEMETRY_REQ} with a
     * 32-byte public key). The reply arrives later as a
     * {@code PUSH_CODE_TELEMETRY_RESPONSE} (0x8B) on
     * {@link SessionListener#onOtherFrame}, or not at all — the peer may be
     * unreachable, may not permit telemetry to us (see the {@code FLAG_*}
     * permission bits on {@link io.iotone.meshcore.model.Contact}), or may
     * simply be asleep. Callers must treat silence as normal.
     *
     * <p>Unlike {@link #requestSelfTelemetry()} this puts a packet on the air
     * and costs the mesh time, so it is a deliberate act per peer and not
     * something to sweep a contact list with.</p>
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     *
     * @param pubKey the peer's full 32-byte public key
     */
    public void requestPeerTelemetry(@NonNull byte[] pubKey) {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.sendTelemetryReq(pubKey));
    }

    /**
     * Requests the device's stored contact list ({@code CMD_GET_CONTACTS},
     * 0x04). The device replies with {@code ContactsStartFrame}, one
     * {@link io.iotone.meshcore.frames.ContactFrame} per contact dispatched to
     * {@link SessionListener#onContact}, then {@code EndOfContactsFrame}.
     *
     * <p>Without this the contact list is never delivered: the device does not
     * push it on connect, so a client that only listens sees an empty mesh
     * while the radio's own screen shows a full one.</p>
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     */
    public void requestContacts() {
        if (state.get() != SessionState.READY) return;
        sendTracked(MeshcoreFrameCodec.getContacts());
    }

    /**
     * Requests one contact by its full public key
     * ({@code CMD_GET_CONTACT_BY_KEY}, 0x1E).
     *
     * <p>The reply is a single {@link io.iotone.meshcore.frames.ContactFrame}
     * on {@link SessionListener#onContact}, or an {@code ErrorFrame} with
     * {@code ERR_CODE_NOT_FOUND}. This is the terminal's connect lookup: it
     * avoids dumping a large contact table into the one answer window.</p>
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     *
     * @param publicKey full 32-byte public key
     */
    public void getContact(@NonNull byte[] publicKey) {
        if (state.get() != SessionState.READY) return;
        try {
            sendTracked(MeshcoreFrameCodec.getContactByKey(publicKey));
        } catch (IllegalArgumentException ignored) {
            // A caller with a malformed key gets no command, not a broken
            // answer window. The adapter only passes a full host key.
        }
    }

    /**
     * Requests battery voltage and storage usage
     * ({@code CMD_GET_BATT_AND_STORAGE}, 0x14). The reply arrives as a
     * {@link io.iotone.meshcore.frames.BatteryStorageFrame} on
     * {@link SessionListener#onOtherFrame}.
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     */
    public void requestBatteryStorage() {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.getBatteryStorage());
    }

    /**
     * Reads one channel slot ({@code CMD_GET_CHANNEL}, 0x1F). The reply
     * arrives as a {@link io.iotone.meshcore.frames.ChannelInfoFrame} on
     * {@link SessionListener#onOtherFrame}.
     *
     * <p>Slot numbering is the firmware's: 0 is conventionally the public
     * channel. A slot that is not configured answers with an empty name, so
     * enumerating channels means asking for each index and reading what comes
     * back — there is no "how many channels" query.</p>
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     *
     * @param channelIdx channel slot index
     */
    public void requestChannel(int channelIdx) {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.getChannel(channelIdx));
    }

    /**
     * Writes one channel slot ({@code CMD_SET_CHANNEL}, 0x20) — name and
     * 16-byte PSK together, because they are one record.
     *
     * <p>THIS OVERWRITES. The firmware keeps one key per slot and takes the
     * last one written; there is no merge and no undo. A slot whose key is
     * replaced stops decrypting everything that was addressed to it, and the
     * old key is gone from the device — recoverable only from wherever else
     * the operator wrote it down. Callers that cannot show the user the key
     * they are about to destroy should refuse to write an occupied slot rather
     * than warn about it.</p>
     *
     * <p>The PSK is the AES-128 key for the channel and the whole of its
     * secrecy. It must not be logged, and the firmware zero-pads it to the
     * 32-byte HMAC secret itself (see
     * {@code ChannelCrypto.channelSecretFromPsk}).</p>
     *
     * <p>Confirmation is a separate read: the device does not echo the slot,
     * so callers that want to verify the write should follow it with
     * {@link #requestChannel(int)}.</p>
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     *
     * @param channelIdx channel slot index
     * @param name       channel name; the codec truncates/pads to 32 bytes
     * @param psk        the 16-byte channel key
     */
    public void setChannel(int channelIdx, String name, byte[] psk) {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.setChannel(channelIdx, name, psk));
    }

    /**
     * Sets the position this node puts in its own adverts
     * ({@code CMD_SET_ADVERT_LATLON}, 0x0C). Degrees, converted to the
     * protocol's micro-degrees.
     *
     * <p>This is what other nodes will see and plot. A node with no GPS
     * reports 0/0 forever, and 0/0 is a real place in the Gulf of Guinea —
     * so an unset position is not a harmless blank, it is a wrong answer
     * that looks like a right one.</p>
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     *
     * @param latitude  degrees
     * @param longitude degrees
     */
    public void setAdvertPosition(double latitude, double longitude) {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.setAdvertLatLon(
                (int) Math.round(latitude * 1_000_000d),
                (int) Math.round(longitude * 1_000_000d)));
    }

    /**
     * Sets the advert location policy ({@code CMD_SET_OTHER_PARAMS}, 0x26)
     * while preserving the other params from {@code selfInfo}.
     *
     * <p>Policy 0 means the node omits its position from adverts however
     * good that position is, so setting a lat/lon without also setting this
     * changes nothing on the air.</p>
     *
     * <p>No-op when not {@link SessionState#READY} or before self-info.</p>
     *
     * @param policy advert location policy (0 = none, 1 = share)
     */
    /**
     * Writes the four LoRa parameters as ONE command
     * ({@code CMD_SET_RADIO_PARAMS}, 0x0B).
     *
     * <p>They are one command because they are one working configuration.
     * Applying them piecemeal would walk the radio through combinations that
     * match no mesh at all, and there is no way back over the air once it stops
     * hearing anything — a radio on the wrong parameters is still connected and
     * still perfectly healthy, it is simply deaf. Callers are expected to stage
     * a complete set and commit it, keeping the previous set for recovery.</p>
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     *
     * @param params the complete parameter set
     */
    public void setRadioParams(RadioParams params) {
        if (state.get() != SessionState.READY) return;
        sendTracked(MeshcoreFrameCodec.setRadioParams(params));
    }

    /**
     * Sets TX power in dBm ({@code CMD_SET_RADIO_TX_POWER}, 0x0C).
     *
     * <p>The ceiling is regulatory as well as physical; callers should clamp to
     * {@code SelfInfo.maxTxPowerDbm()} and to the region's legal limit rather
     * than passing a figure through unchecked.</p>
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     *
     * @param dbm transmit power, dBm
     */
    public void setTxPower(int dbm) {
        if (state.get() != SessionState.READY) return;
        sendTracked(MeshcoreFrameCodec.setRadioTxPower(dbm));
    }

    /**
     * Sets the advertised node name ({@code CMD_SET_ADVERT_NAME}, 0x08).
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     *
     * @param name advertised name; truncated by the codec at 31 bytes
     */
    public void setAdvertName(String name) {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.setAdvertName(name));
    }

    /**
     * Sets who may REQUEST telemetry from this device
     * ({@code CMD_SET_OTHER_PARAMS}, packed byte).
     *
     * <p>The byte carries three 2-bit permission levels — bits 0-1 base
     * (battery), 2-3 location, 4-5 environment — each one of
     * {@code 0 = DENY}, {@code 1 = ALLOW_FLAGS} (per-contact flags on
     * {@link io.iotone.meshcore.model.Contact}), {@code 2 = ALLOW_ALL}.
     * This governs permission, not capability: allowing a class the device
     * has no sensor for simply yields nothing.</p>
     *
     * <p>Like every other setter on this command, it writes the WHOLE record —
     * manual-add, telemetry, location policy and multi-acks together — so the
     * current values of the others are read back from {@link SelfInfo} and
     * resent unchanged. Sending only the field you meant to change silently
     * resets the rest.</p>
     *
     * <p>No-op when not {@link SessionState#READY}.</p>
     *
     * @param packed the packed telemetry permission byte
     */
    public void setTelemetryPermissions(int packed) {
        SelfInfo s = selfInfo();
        if (state.get() != SessionState.READY || s == null) return;
        send(MeshcoreFrameCodec.setOtherParams(
                s.manualAddContacts() ? 1 : 0, packed,
                s.advertLocPolicy(), s.multiAcks()));
    }

    public void setAdvertLocPolicy(int policy) {
        SelfInfo s = selfInfo();
        if (state.get() != SessionState.READY || s == null) return;
        send(MeshcoreFrameCodec.setOtherParams(
                s.manualAddContacts() ? 1 : 0, s.telemetryModeRaw(), policy, s.multiAcks()));
    }

    /**
     * Asks the device what it is: firmware version and build date, the
     * manufacturer string, its BLE pin, and — the field this was finally
     * wired for — {@code maxContacts}, the size of its contact store.
     *
     * The reply is a {@link io.iotone.meshcore.frames.DeviceInfoFrame} on
     * {@link SessionListener#onOtherFrame}. Every part of this has been
     * decodable since the codec was written and unreachable because nothing
     * ever sent the query, which meant an app could watch a contact store fill
     * up and had no way to know how full "full" was.
     *
     * No-op when not {@link SessionState#READY}.
     *
     * @param appTargetVer the app's protocol target version
     */
    public void deviceQuery(int appTargetVer) {
        if (state.get() != SessionState.READY) return;
        sendTracked(MeshcoreFrameCodec.deviceQuery(appTargetVer));
    }

    /**
     * Removes a contact from the RADIO'S store, by full 32-byte public key.
     *
     * The codec has been able to build this frame since it was written and
     * nothing could reach it: a companion radio holds a fixed number of
     * contacts, and a mesh that has been running for months fills that store
     * with nodes heard once from a hilltop. When it is full the radio stops
     * taking new ones — so the node that matters, the one in the same room,
     * cannot be added, and the operator is locked out of their own hardware by
     * a list of strangers. Reported exactly that way, 2026-08-16.
     *
     * NOT A BLOCK, and callers must not present it as one. The contact is
     * forgotten; if that node adverts again it will be learned again, which is
     * usually what makes this safe to do in bulk.
     *
     * No-op when not {@link SessionState#READY}.
     *
     * @param publicKey the contact's full 32-byte public key
     */
    public void removeContact(@NonNull byte[] publicKey) {
        if (state.get() != SessionState.READY) return;
        send(MeshcoreFrameCodec.removeContact(publicKey));
    }

    /**
     * Enables or disables the firmware message-queue drain.
     *
     * <p>On by default for clients that read MeshCore messages. LDMUX turns it
     * off: it has no message consumer, and every drain command shares the
     * companion protocol's single uncorrelated command-answer window.</p>
     *
     * @param enabled true to drain queued messages
     */
    public void setAutoDrain(boolean enabled) {
        autoDrain = enabled;
        if (!enabled) draining = false;
    }

    /** True when no message drain is running and no raw send is pending. */
    public boolean isIdle() {
        synchronized (trackedLock) {
            return !draining && pendingRaw.get() == null && pendingTracked == 0;
        }
    }

    /**
     * Sets the path-hash mode ({@code CMD_SET_PATH_HASH_MODE}, 0x3D).
     *
     * <p>The firmware mode is 0, 1 or 2 for one, two or three bytes per path
     * hash. Preset APIs that call these sizes 1, 2, 3 must subtract one.</p>
     *
     * @param mode path-hash mode (0..2)
     */
    public void setPathHashMode(int mode) {
        if (state.get() != SessionState.READY) return;
        sendTracked(MeshcoreFrameCodec.setPathHashMode(mode));
    }

    /**
     * Sends one complete MeshCore packet ({@code CMD_SEND_RAW_PACKET},
     * 0x41).
     *
     * <p>Use this when the path encoding cannot be expressed by
     * {@code CMD_SEND_RAW_DATA}'s byte-count bounds check, or when the caller
     * needs a route other than direct. The packet is serialised in full,
     * including its header and encoded path.</p>
     *
     * @param priority transmit priority
     * @param packet   complete serialised packet
     * @param callback the one answer for this command
     * @return true when the frame was handed to the transport
     */
    public boolean sendRawPacket(
            int priority,
            @NonNull byte[] packet,
            @NonNull RawSendCallback callback) {
        if (state.get() != SessionState.READY) {
            callback.onResult(false, -1);
            return false;
        }
        final byte[] frame;
        try {
            frame = MeshcoreFrameCodec.sendRawPacket(priority, packet);
        } catch (IllegalArgumentException e) {
            callback.onResult(false, -1);
            return false;
        }
        return sendRawFrame(frame, callback);
    }

    /**
     * Sends one raw-custom packet on a direct path ({@code CMD_SEND_RAW_DATA},
     * 0x19).
     *
     * <p>The companion protocol has no command sequence, so at most one raw
     * send may be in flight. The callback fires on the transport's dispatch
     * thread with the radio's OK/ERR, never with delivery. A timeout must be
     * treated as a dead link, not as permission to send another command that a
     * late answer could resolve.</p>
     *
     * @param path     direct path; empty is a zero-hop direct send
     * @param payload  raw bytes
     * @param callback the one answer for this send
     * @return true when the frame was handed to the transport
     */
    /**
     * Sends one raw-custom packet with an explicit encoded path length.
     *
     * @param pathLen encoded path length (low six bits hops, high two bits
     *                {@code hash_size - 1})
     * @param path    actual path bytes
     * @param payload raw-custom bytes
     * @param callback the one answer for this send
     * @return true when the frame was handed to the transport
     */
    public boolean sendRawData(
            int pathLen,
            @NonNull byte[] path,
            @NonNull byte[] payload,
            @NonNull RawSendCallback callback) {
        if (state.get() != SessionState.READY) {
            callback.onResult(false, -1);
            return false;
        }
        final byte[] frame;
        try {
            frame = MeshcoreFrameCodec.sendRawData(pathLen, path, payload);
        } catch (IllegalArgumentException e) {
            callback.onResult(false, -1);
            return false;
        }
        return sendRawFrame(frame, callback);
    }

    private boolean sendRawFrame(
            @NonNull byte[] frame,
            @NonNull RawSendCallback callback) {
        synchronized (trackedLock) {
            if (pendingTracked != 0) {
                // A config/contact/device answer could still be in flight.
                // Without a sequence number it would be indistinguishable from
                // the raw command's answer, so the raw send waits for quiet.
                callback.onResult(false, -1);
                return false;
            }
            if (!pendingRaw.compareAndSet(null, callback)) {
                callback.onResult(false, -1);
                return false;
            }
        }
        try {
            transport.send(frame);
            return true;
        } catch (IOException | IllegalStateException e) {
            pendingRaw.compareAndSet(callback, null);
            callback.onResult(false, -1);
            pumpTracked();
            return false;
        }
    }

    /**
     * Closes the session and disconnects the transport.
     */
    public void close() {
        RawSendCallback pending = pendingRaw.getAndSet(null);
        if (pending != null) {
            pending.onResult(false, -1);
        }
        synchronized (trackedLock) {
            trackedQueue.clear();
            pendingTracked = 0;
            trackedInFlight = false;
        }
        setState(SessionState.DISCONNECTED);
        try {
            transport.close();
        } catch (IOException ignored) {
        }
    }

    /**
     * Builds a {@code grp_txt_capture} interop-fixture JSON from the
     * most recent RF-log capture (0x88 frame). Drop the returned string
     * as a {@code .json} file into both:
     * <ul>
     *   <li>{@code packages/meshcore/test/vectors/interop/}</li>
     *   <li>{@code libmeshcore/src/test/resources/vectors/interop/}</li>
     * </ul>
     * and run {@code dart test} / {@code ./gradlew test} to validate
     * both libraries simultaneously.
     *
     * @param pskHex              the channel PSK as a lowercase hex string
     *                            (16 bytes = 32 hex chars); use
     *                            {@code "8b3387e9c5cdea6ac9e5edbaa115cd72"}
     *                            for the Public channel
     * @param knownPlaintextUtf8  the exact text the sender transmitted
     *                            (must be a message you can verify arrived)
     * @param firmwareVersion     the flashed firmware version string, for
     *                            fixture provenance ({@code "companion-v1.15.0"})
     * @return fixture JSON, or {@code null} if no 0x88 frame is buffered
     *         yet (send a channel message and wait a moment first)
     */
    @Nullable
    public String exportInteropFixture(
            @NonNull String pskHex,
            @NonNull String knownPlaintextUtf8,
            @NonNull String firmwareVersion) {
        // Walk the RF-log ring newest-first for the last 0x88 frame.
        byte[] lastRfLog = null;
        for (byte[] raw : rfLogCapture) {
            if (raw.length > 0 && raw[0] == (byte) 0x88) {
                lastRfLog = raw;
                break;
            }
        }
        if (lastRfLog == null) return null;

        SelfInfo self = selfInfo;
        return "{\n"
                + "  \"kind\": \"grp_txt_capture\",\n"
                + "  \"description\": \"Public-channel capture from "
                + (self != null ? self.name() : "unknown") + "\",\n"
                + "  \"firmware\": \"" + firmwareVersion + "\",\n"
                + "  \"channel_name\": \"Public\",\n"
                + "  \"psk_hex\": \"" + pskHex + "\",\n"
                + "  \"known_plaintext_utf8\": \"" + knownPlaintextUtf8 + "\",\n"
                + "  \"rf_log_frame_hex\": \"" + bytesToHex(lastRfLog) + "\"\n"
                + "}\n";
    }

    // -------------------------------------------------------------------------
    // Internal: transport listener + frame dispatch
    // -------------------------------------------------------------------------

    private class TransportListener implements MeshcoreTransport.Listener {

        @Override
        public void onFrame(@NonNull byte[] frame) {
            // Keep bytes, not formatted hex. Hex is only needed for an
            // explicit fixture export, not for every raw terminal record.
            if (rfLogCapture.size() >= RF_LOG_BUFFER_CAP) {
                rfLogCapture.pollLast();
            }
            rfLogCapture.addFirst(frame.clone());

            MeshcoreInbound decoded = MeshcoreFrameCodec.decode(frame);
            dispatch(decoded);
        }

        @Override
        public void onConnectionChanged(boolean connected) {
            if (connected) {
                setState(SessionState.HANDSHAKING);
                // Kick off the handshake immediately.
                send(MeshcoreFrameCodec.appStart(appName));
            } else {
                RawSendCallback pending = pendingRaw.getAndSet(null);
                if (pending != null) {
                    pending.onResult(false, -1);
                }
                synchronized (trackedLock) {
                    trackedQueue.clear();
                    pendingTracked = 0;
                    trackedInFlight = false;
                }
                setState(SessionState.DISCONNECTED);
                draining = false;
            }
        }
    }

    private void dispatch(@NonNull MeshcoreInbound frame) {
        if (frame instanceof SelfInfoFrame f) {
            selfInfo = f.selfInfo();
            synchronized (trackedLock) {
                trackedQueue.clear();
                pendingTracked = 0;
                trackedInFlight = false;
            }
            setState(SessionState.READY);
            // Drain any messages that accumulated while we were offline.
            triggerDrain();
            listener.onReady(f.selfInfo());
            return;
        }
        if (frame instanceof MessagesWaitingFrame) {
            triggerDrain();
            return;
        }
        if (frame instanceof NoMoreMessagesFrame) {
            draining = false;
            return;
        }
        if (frame instanceof ChannelMessageFrame f) {
            listener.onChannelMessage(f);
            continueDrain();
            return;
        }
        if (frame instanceof ContactMessageFrame f) {
            listener.onContactMessage(f);
            continueDrain();
            return;
        }
        if (frame instanceof AdvertFrame f) {
            listener.onAdvert(f);
            return;
        }
        if (frame instanceof ContactFrame f) {
            listener.onContact(f);
            continueDrain();
            return;
        }
        if (frame instanceof RawDataFrame f) {
            listener.onRawData(f);
            // LDMUX disables auto-drain; clients that keep it on still need
            // the message queue to advance after a raw push.
            if (autoDrain) continueDrain();
            return;
        }
        if (frame instanceof OkFrame || frame instanceof ErrorFrame) {
            RawSendCallback pending = pendingRaw.getAndSet(null);
            if (pending != null) {
                if (frame instanceof OkFrame) {
                    pending.onResult(true, -1);
                } else {
                    ErrorFrame e = (ErrorFrame) frame;
                    pending.onResult(false, e.code().isPresent() ? e.code().getAsInt() : -1);
                }
                pumpTracked();
            } else {
                trackedAnswered();
            }
        }
        if (frame instanceof io.iotone.meshcore.frames.EndOfContactsFrame) {
            trackedAnswered();
        }
        if (frame instanceof io.iotone.meshcore.frames.DeviceInfoFrame) {
            trackedAnswered();
        }
        if (frame instanceof DecodeFailure f) {
            listener.onDecodeFailure(f);
            return;
        }
        // Everything else (BatteryStorageFrame, TelemetryResponse…).
        listener.onOtherFrame(frame);
        continueDrain();
    }

    /**
     * Start the sync-drain loop if not already draining and the session
     * is ready.
     */
    private void triggerDrain() {
        if (autoDrain && !draining && state.get() == SessionState.READY) {
            draining = true;
            if (!send(MeshcoreFrameCodec.syncNextMessage())) draining = false;
        }
    }

    /**
     * Issue the next {@code SYNC_NEXT_MESSAGE} if a drain is in progress.
     */
    private void continueDrain() {
        if (autoDrain && draining) {
            if (!send(MeshcoreFrameCodec.syncNextMessage())) draining = false;
        }
    }

    private void setState(@NonNull SessionState next) {
        SessionState prev = state.getAndSet(next);
        if (prev != next) {
            listener.onStateChanged(next);
        }
    }

    private void sendTracked(@NonNull byte[] frame) {
        synchronized (trackedLock) {
            if (pendingTracked >= MAX_TRACKED_QUEUE) {
                // A caller that queues without waiting is misusing the API.
                // Dropping here is safer than writing an answer nobody can
                // correlate. The preset path sends three and waits for idle.
                return;
            }
            pendingTracked++;
            trackedQueue.addLast(frame);
        }
        pumpTracked();
    }

    private void trackedAnswered() {
        synchronized (trackedLock) {
            if (pendingTracked == 0) return;
            pendingTracked--;
            trackedInFlight = false;
        }
        pumpTracked();
    }

    /**
     * Writes the next tracked command only when the one answer slot is free:
     * no raw send waiting and no tracked command in flight.
     */
    private void pumpTracked() {
        final byte[] next;
        synchronized (trackedLock) {
            if (trackedInFlight || pendingRaw.get() != null) return;
            next = trackedQueue.pollFirst();
            if (next == null) return;
            trackedInFlight = true;
        }
        if (!send(next)) trackedAnswered();
    }

    private static final int MAX_TRACKED_QUEUE = 16;

    private boolean send(@NonNull byte[] frame) {
        try {
            transport.send(frame);
            return true;
        } catch (IOException | IllegalStateException e) {
            // Link dropped mid-session; the connection-observer will fire
            // onConnectionChanged(false) and reset the state.
            return false;
        }
    }


    private static String bytesToHex(@NonNull byte[] bytes) {
        final char[] digits = "0123456789abcdef".toCharArray();
        char[] out = new char[bytes.length * 2];
        for (int i = 0; i < bytes.length; i++) {
            int v = bytes[i] & 0xFF;
            out[i * 2] = digits[v >>> 4];
            out[i * 2 + 1] = digits[v & 0x0F];
        }
        return new String(out);
    }
}
