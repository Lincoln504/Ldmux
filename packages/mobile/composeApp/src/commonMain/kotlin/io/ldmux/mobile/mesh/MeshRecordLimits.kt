package io.ldmux.mobile.mesh

/**
 * The record sizes the L2 layer enforces, mirrored from `ldmux_mesh::constants`.
 *
 * One place rather than three: the floor decides `NoPath`, the ceiling is handed to the session
 * as its record limit, and both were written out separately in this package (the radio link, the
 * ABI wrapper and the no-radio placeholder), which is three chances to disagree with Rust. The
 * fixture `test/fixtures/mesh-term-vectors.json` carries the Rust values, and
 * `MeshTermCodecTest.theLimitsAreTheHostsLimits` compares these against them.
 */
internal object MeshRecordLimits {
    /**
     * The smallest limit worth sending on: **a sanity floor, not a path test.**
     *
     * The firmware's longest legal path is 64 bytes, so the limit can never legitimately fall below
     * `MAX - 64` = 108. It was 160 — what a 12-byte path leaves, so six two-byte hops — which made
     * the phone refuse to send *anything* on a seven-hop route and report "no usable path to host"
     * on a link that was working.
     */
    const val FLOOR = 108

    /** The absolute ceiling: the wire budget with an empty path. */
    const val MAX = 172

    /**
     * The most a radio will hand up as a receive-log push: 171.
     *
     * A radio does not push a frame it cannot fit in its 176-byte buffer, so a 172-byte record is
     * transmitted, received, and never reported — silently, on every full record.
     */
    const val RECEIVED_MAX = 171

    /** Records the session may keep outstanding: the radio's credit window. */
    const val CREDIT_WINDOW = 8
}
