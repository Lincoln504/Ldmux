package dev.ldmux.mobile.mesh

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
    /** The conservative floor: a long path still fits a record this size. */
    const val FLOOR = 160

    /** The absolute ceiling, as advertised per peer in `BIND_OK`. */
    const val MAX = 172

    /** Records the session may keep outstanding: the radio's credit window. */
    const val CREDIT_WINDOW = 8
}
