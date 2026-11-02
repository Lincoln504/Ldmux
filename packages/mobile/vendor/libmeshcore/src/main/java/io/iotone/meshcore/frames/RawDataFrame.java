// Copyright (c) 2026 IoTone, Inc.
// SPDX-License-Identifier: MIT
package io.iotone.meshcore.frames;

/**
 * {@code PUSH_CODE_RAW_DATA} (0x84): one raw-custom payload received by the
 * radio.
 *
 * <p>Wire form: {@code [0x84][SNR int8, quarter-dB][RSSI int8][0xFF][payload]}.
 * The firmware does not decrypt or interpret the payload; it forwards the
 * bytes the sender put in {@code CMD_SEND_RAW_DATA}. There is <strong>no
 * sender identity</strong> in this frame, so the application layer must
 * authenticate the payload.</p>
 *
 * @param snrDb signal-to-noise ratio in dB (quarter-dB byte divided by 4)
 * @param rssi  received signal strength in dBm (signed byte)
 * @param payload raw-custom bytes, copied
 */
public record RawDataFrame(double snrDb, int rssi, byte[] payload)
        implements MeshcoreInbound {

    /**
     * Returns a defensive copy of the payload.
     *
     * @return the raw payload bytes
     */
    @Override
    public byte[] payload() {
        return payload.clone();
    }

    @Override
    public String toString() {
        return "RawDataFrame(snr: " + snrDb + " dB, rssi: " + rssi
                + " dBm, " + payload.length + "B)";
    }
}
