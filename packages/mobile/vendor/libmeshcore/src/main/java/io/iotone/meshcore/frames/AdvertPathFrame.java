// Copyright (c) 2026 IoTone, Inc.
// SPDX-License-Identifier: MIT
package io.iotone.meshcore.frames;

/**
 * {@code RESP_CODE_ADVERT_PATH} (0x16) — the accumulated path of a recently
 * heard advert, read out of the firmware's advert-path cache.
 *
 * <p>Wire layout:
 * {@code [0x16][received at, u32 LE][path_len u8][path bytes]}. The
 * {@code path_len} byte is the firmware's own encoded form
 * ({@code ((hashSize - 1) << 6) | hopCount}), not a byte count, and exactly
 * {@code hopCount * hashSize} bytes follow it.</p>
 *
 * <p><strong>The path is in travel order towards us.</strong> A flooded advert
 * collects one hash per repeater as it travels, so this holds the route the
 * advert took to arrive here. Sending <em>to</em> the advert's originator uses
 * the same hops in the opposite order; reverse the groups, not the bytes.</p>
 *
 * <p>This is the only route a companion can learn without a contact and without
 * a decrypted path return: the advert is unencrypted, its path cache is filled
 * by every advert the radio hears, and this command reads it out.</p>
 *
 * @param receivedAtSeconds advert's own timestamp, as the firmware stored it
 * @param pathLength        encoded path length ({@code ((hashSize-1)<<6)|count})
 * @param path              the path bytes, in travel order; copied
 */
public record AdvertPathFrame(int receivedAtSeconds, int pathLength, byte[] path)
        implements MeshcoreInbound {

    /** Hops in the path, from the encoded length byte. */
    public int hopCount() {
        return pathLength & 0x3F;
    }

    /** Bytes per hop (1..4), from the encoded length byte. */
    public int hashSize() {
        return ((pathLength >> 6) & 0x03) + 1;
    }

    /** Returns a defensive copy of the path. */
    @Override
    public byte[] path() {
        return path.clone();
    }

    @Override
    public String toString() {
        return "AdvertPathFrame(hops=" + hopCount() + ", hashSize=" + hashSize()
                + ", bytes=" + path.length + ", at=" + receivedAtSeconds + ")";
    }
}
