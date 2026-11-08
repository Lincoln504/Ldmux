// Copyright (c) 2026 IoTone, Inc.
// SPDX-License-Identifier: MIT
package io.iotone.meshcore.frames;

/**
 * {@code RESP_CODE_DEFAULT_FLOOD_SCOPE} (0x1C) — the radio's **default flood scope**, the reply to
 * {@code CMD_GET_DEFAULT_FLOOD_SCOPE} (0x40).
 *
 * <p>Wire form, from the firmware's handler ({@code MyMesh.cpp:1964-1972}):</p>
 *
 * <pre>
 * [0x1C]                       the radio has no default scope (its "null" answer)
 * [0x1C][name 31][key 16]      the region name, NUL-padded, and the 16-byte transport key
 * </pre>
 *
 * <p>A region's key is {@code SHA256("#" + name)} cut to 16 bytes for the public hashtag-style
 * regions everyone uses, so the key is derivable from the name for those. A private region (a name
 * beginning {@code $}) has a stored key instead, which is why the key is carried here: it is the
 * only way a client can learn it.</p>
 *
 * @param name the region name as the radio stores it, without the leading {@code #}; empty when
 *             the radio has no default scope
 * @param key  the 16-byte transport key, or {@code null} when there is no scope
 */
public record DefaultFloodScopeFrame(String name, byte[] key) implements MeshcoreInbound {

    /** The name, or {@code null} when the radio has no default scope. */
    public String scopeName() {
        return name.isEmpty() ? null : name;
    }

    /** A defensive copy of the key, or {@code null} when the radio has no default scope. */
    public byte[] scopeKey() {
        return key == null ? null : key.clone();
    }

    @Override
    public String toString() {
        return "DefaultFloodScopeFrame(" + (name.isEmpty() ? "none" : name) + ")";
    }
}
