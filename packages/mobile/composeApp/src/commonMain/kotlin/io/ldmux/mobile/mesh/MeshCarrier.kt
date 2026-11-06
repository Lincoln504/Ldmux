package io.ldmux.mobile.mesh

/**
 * Where the mesh link is. One value, so the UI never infers it from several
 * booleans. The token, gateway, busy and version states are gone: the phone
 * owns the radio and the only failures left are the radio's own.
 */
enum class MeshLinkState {
    /** No radio selected yet. Nothing to connect with. */
    NeedsRadio,

    /** Android refused the BLE permission needed to reach the radio. */
    NeedsPermission,

    /** The host radio has no direct path that can carry the session's record floor. */
    NoPath,

    Idle,
    Connecting,

    /** The radio answered APP_START and the session is ready. */
    Bound,

    /** Bound, but the radio is not connected, or the transport ended. */
    RadioUnavailable,

    /**
     * The end-to-end session beneath a bound link gave up: the host stopped
     * answering, or the native session could not run. The radio may be fine.
     */
    SessionFailed,

    /** The radio refused the record (not a contact, or an unsupported command). */
    Refused,

    /** Closed by the radio or by us. Reconnect is the caller's decision, never automatic. */
    Closed,
}

/** The answer to one carrier send. What the radio did, **never** delivery. */
data class MeshSendResult(val seq: Int, val outcome: MeshSendOutcome)

/** What the radio did with a record. **Not** delivery: a blind carrier cannot know that. */
enum class MeshSendOutcome {
    SentDirect,
    SentFlood,
    ErrNotFound,
    ErrBusy,
    ErrTooLong,
    ErrBudget,
    LocalTimeout,
    Unknown,
}

/** The radio's status, as far as the terminal needs it. */
data class MeshStatus(
    val radioState: Int,
    val rssiDbm: Int?,
    /** The radio's own transmit airtime this hour, 0.1 s units. An estimate, display only. */
    val txAirtimeTenthsThisHour: Int,
    /**
     * Records of the current uplink size that still fit this hour's budget
     * (0..255, 255 means "255 or more"). Null when the source cannot compute
     * it. LDMUX never derives it: that would need radio parameters.
     */
    val opsLeftThisHour: Int? = null,
) {
    val radioConnected: Boolean get() = radioState == RADIO_CONNECTED

    companion object {
        /** The firmware's `RADIO_CONNECTED` code, as `SELF_INFO`/status report it. */
        const val RADIO_CONNECTED = 0

        /**
         * Nothing has said anything about the radio yet.
         *
         * The status used to start at [RADIO_CONNECTED], so a link that had never opened a GATT
         * client reported a connected radio. A fact nobody has established is not a fact.
         */
        const val RADIO_UNKNOWN = -1
    }
}
