// Copyright (c) 2026 IoTone, Inc.
// SPDX-License-Identifier: MIT
package io.iotone.meshcore.model;

import java.util.Arrays;

/**
 * Decoded {@code RESP_CODE_CONTACT} (0x03).
 *
 * <p>Fixed 148-byte frame, layout transcribed from
 * {@code examples/companion_radio/MyMesh.cpp} (pinned commit):</p>
 *
 * <pre>
 * [0]        0x03
 * [1..32]    pub_key            (32, PUB_KEY_SIZE)
 * [33]       type
 * [34]       flags
 * [35]       out_path_len
 * [36..99]   out_path           (64, MAX_PATH_SIZE)
 * [100..131] name               (32)
 * [132..135] last_advert_ts     (uint32 LE)
 * [136..139] gps_lat            (int32  LE, &#247;1e6)
 * [140..143] gps_lon            (int32  LE, &#247;1e6)
 * [144..147] lastmod            (uint32 LE)
 * </pre>
 *
 * <p>Lat/lon are kept as the raw signed micro-degrees so a decoded
 * contact re-encodes byte-exactly via ADD_UPDATE_CONTACT; use
 * {@link #latitude()} / {@link #longitude()} for degrees. Arrays are
 * owned by the record — callers must not mutate them.</p>
 *
 * @param publicKey           32-byte Ed25519 public key (full identity)
 * @param type                contact node type
 * @param flags               contact flags (see the {@code FLAG_*}
 *                            constants)
 * @param outPathLen          encoded path length: low 6 bits are the hop
 *                            count, high 2 bits are {@code hash_size - 1};
 *                            {@code 0xFF} means the path is unknown. This is
 *                            not a byte count (the buffer is always 64 bytes).
 * @param outPath             64-byte outbound path buffer
 * @param name                contact display name (NUL-trimmed)
 * @param lastAdvertTimestamp unix seconds of the contact's last advert
 * @param latitudeMicros      raw signed micro-degrees (on-wire int32)
 * @param longitudeMicros     raw signed micro-degrees (on-wire int32)
 * @param lastMod             modification counter used for incremental
 *                            contact sync
 */
public record Contact(
        byte[] publicKey,
        int type,
        int flags,
        int outPathLen,
        byte[] outPath,
        String name,
        long lastAdvertTimestamp,
        int latitudeMicros,
        int longitudeMicros,
        long lastMod) {

    // --- `flags` bit layout (MeshCore companion firmware) ----------------
    // The firmware reads telemetry permission as `cp = flags >> 1`, then
    // `cp & TELEM_PERM_{BASE=0x01,LOCATION=0x02,ENVIRONMENT=0x04}` — so
    // the permission bits live one position up from the LSB favourite
    // bit. (Source: examples/companion_radio/MyMesh.cpp onContactRequest
    // + src/helpers/SensorManager.h.)

    /** Flags bit: device-side favourite. */
    public static final int FLAG_FAVOURITE = 0x01;

    /** Flags bit: contact may request base (battery) telemetry. */
    public static final int FLAG_TELEM_BASE = 0x02;

    /** Flags bit: contact may request location telemetry. */
    public static final int FLAG_TELEM_LOCATION = 0x04;

    /** Flags bit: contact may request environment telemetry. */
    public static final int FLAG_TELEM_ENVIRONMENT = 0x08;

    /**
     * Returns the latitude in degrees (raw &#247; 1e6).
     *
     * @return latitude degrees
     */
    public double latitude() {
        return latitudeMicros / 1e6;
    }

    /**
     * Returns the longitude in degrees (raw &#247; 1e6).
     *
     * @return longitude degrees
     */
    public double longitude() {
        return longitudeMicros / 1e6;
    }

    /** The sentinel the firmware uses when it has no path for this contact. */
    public static final int OUT_PATH_UNKNOWN = 0xFF;

    /** True when the firmware knows a path. Zero hops is a known path. */
    public boolean hasPath() {
        return outPathLen != OUT_PATH_UNKNOWN;
    }

    /** Hash width in bytes: 1, 2 or 3. Zero when the path is unknown or invalid. */
    public int pathHashSize() {
        if (!hasPath()) return 0;
        int encoded = (outPathLen >>> 6) & 0x03;
        return encoded == 3 ? 0 : encoded + 1;
    }

    /** Number of hops in the path, or zero when it is unknown or invalid. */
    public int pathHopCount() {
        if (!hasPath()) return 0;
        return outPathLen & 0x3F;
    }

    /** Number of path bytes actually present ({@code hops * hash_size}). */
    public int pathByteCount() {
        int size = pathHashSize();
        return size == 0 ? 0 : pathHopCount() * size;
    }

    /** True when the encoding uses the reserved hash-size code 3 or the reserved 0xFF. */
    public boolean hasValidPathEncoding() {
        return hasPath() && pathHashSize() != 0;
    }

    /**
     * Returns the path bytes in use, as a fresh copy.
     *
     * <p>{@code outPathLen} is an encoded byte, not a count of bytes. A
     * two-byte hash path of one hop is encoded as {@code 0x41}, so slicing
     * {@code outPath[0..outPathLen)} would return the whole 64-byte buffer.
     * This method decodes the hop count and hash width first.</p>
     *
     * <p>A zero-hop path and an unknown path both return an empty array.
     * Callers that need to distinguish them must check {@link #hasPath()}.
     * An invalid encoding also returns empty.</p>
     *
     * @return active path bytes
     */
    public byte[] activePath() {
        if (!hasValidPathEncoding()) return new byte[0];
        int n = pathByteCount();
        if (n > outPath.length) return new byte[0];
        return Arrays.copyOfRange(outPath, 0, n);
    }

    /**
     * Returns whether this contact may request <strong>base</strong>
     * (battery) telemetry from us when the device telemetry mode is
     * "Contacts" (ALLOW_FLAGS).
     *
     * @return {@code true} when the base-telemetry flag is set
     */
    public boolean allowsTelemBase() {
        return (flags & FLAG_TELEM_BASE) != 0;
    }

    /**
     * Returns whether this contact may request location telemetry.
     *
     * @return {@code true} when the location-telemetry flag is set
     */
    public boolean allowsTelemLocation() {
        return (flags & FLAG_TELEM_LOCATION) != 0;
    }

    /**
     * Returns whether this contact may request environment telemetry.
     *
     * @return {@code true} when the environment-telemetry flag is set
     */
    public boolean allowsTelemEnvironment() {
        return (flags & FLAG_TELEM_ENVIRONMENT) != 0;
    }

    /**
     * Returns whether the device marks this contact a favourite.
     *
     * @return {@code true} when the favourite flag is set
     */
    public boolean isDeviceFavourite() {
        return (flags & FLAG_FAVOURITE) != 0;
    }

    /**
     * Returns a copy of this contact with different {@code flags}
     * (everything else unchanged) — for round-tripping a flags edit
     * through ADD_UPDATE_CONTACT.
     *
     * @param newFlags replacement flags byte
     * @return a new contact value
     */
    public Contact withFlags(int newFlags) {
        return new Contact(publicKey, type, newFlags, outPathLen, outPath,
                name, lastAdvertTimestamp, latitudeMicros, longitudeMicros,
                lastMod);
    }

    @Override
    public String toString() {
        return "Contact(name: " + name + ", type: " + type + ", pathLen: "
                + outPathLen + ", lastMod: " + lastMod + ")";
    }
}
