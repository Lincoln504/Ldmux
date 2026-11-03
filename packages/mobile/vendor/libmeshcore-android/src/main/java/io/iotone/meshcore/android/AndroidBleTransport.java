// Copyright (c) 2026 IoTone, Inc.
// SPDX-License-Identifier: MIT
package io.iotone.meshcore.android;

import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattService;
import android.content.Context;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;

import androidx.annotation.NonNull;
import androidx.annotation.Nullable;

import java.io.IOException;
import java.util.UUID;

import io.iotone.meshcore.MeshcoreBle;
import io.iotone.meshcore.transport.MeshcoreTransport;
import no.nordicsemi.android.ble.BleManager;
import no.nordicsemi.android.ble.observer.ConnectionObserver;

/**
 * Android BLE transport for the MeshCore companion-radio protocol,
 * backed by the Nordic Android BLE Library ({@code no.nordicsemi.android:ble}).
 *
 * <p>The MeshCore companion link uses a Nordic-UART-style BLE service
 * (UUIDs in {@link MeshcoreBle}): the app <em>writes</em> command frames
 * to the RX characteristic and <em>subscribes</em> to notifications on
 * the TX characteristic — one write and one notification = one complete
 * protocol frame.</p>
 *
 * <p>This class composes a private {@link BleManager} subclass rather
 * than extending it directly. This avoids conflicts with the several
 * {@code final} methods on {@link BleManager} ({@code isConnected()},
 * {@code close()}) that would shadow the identically-named
 * {@link MeshcoreTransport} interface methods.</p>
 *
 * <h3>Usage</h3>
 * <pre>{@code
 * AndroidBleTransport transport = new AndroidBleTransport(context);
 * MeshcoreSession session = new MeshcoreSession("MyApp", transport, listener);
 * transport.connectToDevice(device);   // scanning is the caller's job
 * }</pre>
 *
 * <p>All callbacks are dispatched on the Android main thread.</p>
 */
public final class AndroidBleTransport implements MeshcoreTransport {

    /** Requested MTU — fits any MeshCore frame with room to spare. */
    private static final int PREFERRED_MTU = 256;

    /** ATT overhead plus the largest companion frame. */
    private static final int MIN_MTU = 176 + 3;

    private static final UUID SERVICE_UUID =
            UUID.fromString(MeshcoreBle.SERVICE_UUID);
    private static final UUID RX_UUID =
            UUID.fromString(MeshcoreBle.RX_CHARACTERISTIC_UUID);
    private static final UUID TX_UUID =
            UUID.fromString(MeshcoreBle.TX_CHARACTERISTIC_UUID);

    private volatile Listener frameListener;
    private final Manager manager;

    /**
     * Creates a transport. Pass the Application {@code Context} for
     * long-lived sessions (do not pass an Activity context).
     *
     * @param context application context
     */
    public AndroidBleTransport(@NonNull Context context) {
        manager = new Manager(context);
    }

    // -------------------------------------------------------------------------
    // MeshcoreTransport
    // -------------------------------------------------------------------------

    @Override
    public void setListener(@Nullable Listener listener) {
        this.frameListener = listener;
    }

    /**
     * Returns {@code true} when the BLE connection is established AND the
     * MeshCore GATT profile has been fully initialised (TX notifications
     * enabled, MTU negotiated). Only then may {@link #send} be called.
     *
     * @return {@code true} when ready for protocol traffic
     */
    @Override
    public boolean isConnected() {
        return manager.isConnected() && manager.isReady()
                && manager.mtu() >= MIN_MTU;
    }

    /**
     * Sends exactly one protocol frame (one BLE {@code WRITE_NO_RESPONSE}
     * to the RX characteristic). Writes are queued by the Nordic library
     * and execute sequentially.
     *
     * @param frame a single complete protocol frame
     * @throws IllegalStateException if the transport is not connected
     * @throws IOException           (declared for the interface contract)
     */
    @Override
    public void send(@NonNull byte[] frame) throws IOException {
        if (!isConnected()) {
            throw new IllegalStateException(
                    "Not connected to a MeshCore device");
        }
        manager.sendFrame(frame);
    }

    /**
     * Disconnects from the radio and releases resources. Safe to call
     * from any thread.
     *
     * @throws IOException never (declared for the interface contract)
     */
    @Override
    public void close() throws IOException {
        manager.shutdown();
    }

    // -------------------------------------------------------------------------
    // Public helpers
    // -------------------------------------------------------------------------

    /**
     * Initiates a BLE connection to {@code device}. Call this after
     * the BLE scan has located the target radio and you have a
     * {@link BluetoothDevice} handle. The session listener receives
     * {@link Listener#onConnectionChanged onConnectionChanged(true)} when
     * the connection is ready to use.
     *
     * @param device the target BLE device
     */
    public void connectToDevice(@NonNull BluetoothDevice device) {
        manager.connect(device)
                .useAutoConnect(false)
                .retry(3, 200)
                .timeout(12_000)
                .enqueue();
    }

    // -------------------------------------------------------------------------
    // Inner BleManager — composition avoids final-method shadowing
    // -------------------------------------------------------------------------

    private final class Manager extends BleManager {

        private BluetoothGattCharacteristic rxCharacteristic;
        private BluetoothGattCharacteristic txCharacteristic;

        Manager(@NonNull Context context) {
            super(context);
            setConnectionObserver(new ConnectionObserver() {
                @Override
                public void onDeviceConnecting(@NonNull BluetoothDevice d) {
                }

                @Override
                public void onDeviceConnected(@NonNull BluetoothDevice d) {
                }

                @Override
                public void onDeviceFailedToConnect(
                        @NonNull BluetoothDevice d, int reason) {
                    notifyConnectionChanged(false);
                }

                /**
                 * Fires after {@code initialize()} returns, which is before the
                 * queued MTU and notification requests complete. Readiness is
                 * reported from the notification-enable request instead, so
                 * APP_START cannot be written to an unsubscribed link.
                 */
                @Override
                public void onDeviceReady(@NonNull BluetoothDevice d) {
                }

                @Override
                public void onDeviceDisconnecting(@NonNull BluetoothDevice d) {
                }

                @Override
                public void onDeviceDisconnected(
                        @NonNull BluetoothDevice d, int reason) {
                    notifyConnectionChanged(false);
                }
            });
        }

        int mtu() {
            return getMtu();
        }

        void sendFrame(@NonNull byte[] frame) {
            if (rxCharacteristic == null) {
                notifyConnectionChanged(false);
                return;
            }
            int properties = rxCharacteristic.getProperties();
            int writeType;
            if ((properties & BluetoothGattCharacteristic.PROPERTY_WRITE) != 0) {
                // Write with response is the portable choice. The ESP32
                // companion advertises PROPERTY_WRITE only, and Android rejects
                // a write-no-response against such a characteristic.
                writeType = BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT;
            } else if ((properties
                    & BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE) != 0) {
                writeType = BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE;
            } else {
                notifyConnectionChanged(false);
                return;
            }
            writeCharacteristic(rxCharacteristic, frame, writeType)
                    .fail((device, status) -> notifyConnectionChanged(false))
                    .enqueue();
        }

        void shutdown() {
            // close() disconnects and releases the GATT/notification state.
            // disconnect().enqueue() alone left the old manager alive across
            // reconnects.
            close();
        }

        // --- BleManager overrides ---

        @Override
        protected boolean isRequiredServiceSupported(
                @NonNull BluetoothGatt gatt) {
            BluetoothGattService service = gatt.getService(SERVICE_UUID);
            if (service == null) {
                StringBuilder sb = new StringBuilder("ldmux-ble services:");
                for (BluetoothGattService svc : gatt.getServices()) {
                    sb.append(' ').append(svc.getUuid());
                }
                Log.d("ldmux", sb.toString());
                return false;
            }
            rxCharacteristic = service.getCharacteristic(RX_UUID);
            txCharacteristic = service.getCharacteristic(TX_UUID);
            return rxCharacteristic != null && txCharacteristic != null;
        }

        private final Handler main = new Handler(Looper.getMainLooper());
        private static final int BOND_POLL_MS = 100;
        private static final int BOND_POLLS = 300;
        private static final int BOND_RETRY_POLL = 20;

        @Override
        protected void initialize() {
            // Bond after service discovery, not before connecting. Android
            // needs the live GATT link for createBond, and the radio's
            // characteristics are encrypted, so notifications must wait for
            // the bond to be usable.
            //
            // Do NOT use ensureBond(): Nordic's ENSURE_BOND enqueues a
            // removeBond when createBond() returns false (already bonding, or
            // a bond created concurrently). That removed a live bond on the
            // real WIO and left the link at status 22. This path only ever
            // creates a bond and waits for BONDED.
            awaitBond(getBluetoothDevice(), 0);
        }

        private void awaitBond(@Nullable BluetoothDevice device, int tries) {
            if (device == null) {
                notifyConnectionChanged(false);
                return;
            }
            int state = device.getBondState();
            if (state == BluetoothDevice.BOND_BONDED) {
                negotiateMtuAndSubscribe();
                return;
            }
            if (state == BluetoothDevice.BOND_BONDING) {
                if (tries >= BOND_POLLS) {
                    notifyConnectionChanged(false);
                    return;
                }
                main.postDelayed(() -> awaitBond(device, tries + 1), BOND_POLL_MS);
                return;
            }
            if (tries == 0 || tries == BOND_RETRY_POLL) {
                createBond()
                        .done((d) -> awaitBond(device, tries + 1))
                        .fail((d, status) -> awaitBond(device, tries + 1))
                        .enqueue();
            } else if (tries >= BOND_POLLS) {
                notifyConnectionChanged(false);
            } else {
                main.postDelayed(() -> awaitBond(device, tries + 1), BOND_POLL_MS);
            }
        }

        private void negotiateMtuAndSubscribe() {
            requestMtu(PREFERRED_MTU)
                    .done((device) -> {
                        if (getMtu() < MIN_MTU) {
                            notifyConnectionChanged(false);
                        } else {
                            subscribe();
                        }
                    })
                    .fail((device, status) -> {
                        Log.d("ldmux", "ble mtu request failed status=" + status + " mtu=" + getMtu());
                        // Some stacks refuse the request but already carry a
                        // large ATT MTU. Use it only when the ceiling is real.
                        if (getMtu() >= MIN_MTU) {
                            subscribe();
                        } else {
                            notifyConnectionChanged(false);
                        }
                    })
                    .enqueue();
        }

        private void subscribe() {
            setNotificationCallback(txCharacteristic)
                    .with((device, data) -> {
                        byte[] raw = data.getValue();
                        if (raw == null || raw.length == 0) return;
                        Listener l = frameListener;
                        if (l != null) l.onFrame(raw.clone());
                    });
            // This is the readiness point. onDeviceReady() runs when
            // initialize() returns, before bonding, MTU and notifications have
            // completed, and MeshcoreSession sends APP_START on that signal.
            enableNotifications(txCharacteristic)
                    .done((device) -> notifyConnectionChanged(true))
                    .fail((device, status) -> {
                        Log.d("ldmux", "ble subscribe failed status=" + status);
                        notifyConnectionChanged(false);
                    })
                    .enqueue();
        }

        @Override
        protected void onServicesInvalidated() {
            rxCharacteristic = null;
            txCharacteristic = null;
        }
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

    private void notifyConnectionChanged(boolean connected) {
        Listener l = frameListener;
        if (l != null) l.onConnectionChanged(connected);
    }
}
