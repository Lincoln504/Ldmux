// Copyright (c) 2026 IoTone, Inc.
// SPDX-License-Identifier: MIT
package io.iotone.meshcore.android;

/**
 * The radio's answer to one {@code CMD_SEND_RAW_DATA}.
 *
 * <p>This is what the radio accepted, <strong>not delivery</strong>. The
 * companion protocol answers OK or ERR for the command; a sent raw packet
 * can still be lost on the air. LDMUX's ARQ owns delivery.</p>
 */
public interface RawSendCallback {

    /**
     * Called exactly once for a send that was accepted into the session.
     *
     * @param ok        true for {@code RESP_CODE_OK}, false for
     *                  {@code RESP_CODE_ERR} or a failed transport write
     * @param errorCode the firmware error byte when {@code ok} is false and
     *                  the radio supplied one, else {@code -1}
     */
    void onResult(boolean ok, int errorCode);
}
