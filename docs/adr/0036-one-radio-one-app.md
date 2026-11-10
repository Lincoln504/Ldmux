# 0036 — One radio, one app: the lease, because Android will not arbitrate

- **Status:** accepted
- **Date:** 2026-11-10

## Context

The phone has **one** MeshCore BLE companion, and at least two apps that want it: ldmux and
Meshmore, the user's own radio app.

A MeshCore companion accepts exactly **one** BLE central. That is firmware design, not a setting:
the ESP and nRF stacks can do more (NimBLE-Arduino defaults to three, ESP-IDF supports nine), but
the companion firmware exposes no connection-count knob and no connection-parameter surface. The
documented behaviour is worse than a refusal: **the second central often completes the connection and
is dropped later, when the first one next writes.** A refusal at connect time would be a clear error.
This is a link that works for a few seconds and then vanishes.

Android offers nothing here. There is no API by which one app can close another app's `BluetoothGatt`
connection, and no OS-level arbitration between apps that want the same peripheral. The only
supported arrangement is an agreement between the apps, or a single owning app.

Measured on the device, 2026-11-10: while ldmux was mid-handshake, Meshmore was registering a GATT
client, connecting to the companion, and dropping the link **every thirty seconds** (three
`DISCONNECTED … reason=22` in one minute). From the operator's side — walking a range test — that
reads as "handshake, fail, handshake, fail, sometimes the link dropped", on a radio that was working.

There is a second reason to keep BLE reconnections rare, and it is not ours to fix: firmware 1.17.1
on ESP32-S3 Heltec boards has a reported defect where a BLE **disconnect and reconnect** leaves the
LoRa side deaf and mute — BLE still connects, the LED still advertises, and the node neither hears nor
is heard (MeshCore issue #3582, reproduced on two phones, cured only by reflashing). Every reconnect
is a lottery ticket against that.

## Decision

**Two broadcasts, one owner at a time, and nobody stops running.**

- `io.ldmux.mobile.action.RADIO_WANTED` — sent by the app that needs the radio, explicitly addressed
  (`setPackage`) to the known holder. Extras: `requested_by`.
- `io.ldmux.mobile.action.RADIO_GRANTED` — the holder's answer, back to `requested_by`. Extras:
  `granted_by`.
- `io.ldmux.mobile.action.RADIO_FREE` — sent when the requester is done, to the same packages.

Rules both sides implement:

1. **The holder yields, it does not exit.** On `WANTED` the holder persists `yielded = true`, tells
   its own link layer to disconnect, and answers `GRANTED`. Its foreground service, notification and
   pending work stay exactly as they were.
2. **The holder stays silent while yielded.** Its reconnect ladder is suppressed — not delayed — so
   the requester is not evicted a minute later by a retry that predates the request.
3. **The requester asks before it connects, and frees when it is done.** ldmux sends `WANTED` before
   touching the adapter and `FREE` in `close()`. It waits up to three seconds for `GRANTED`, because a
   connect that races a link the firmware has not finished dropping fails as status 133.
4. **Absence means free.** No answer within the window is the normal case with the radio idle, and it
   proceeds. A counterpart that is not installed, was killed, or never implemented the contract holds
   nothing, so silence must never block.
5. **The flag outlives the process.** `yielded` is persisted by the holder and read at launch, so a
   request that arrives while the app is dead is still honoured when it starts.
6. **An operator's own action beats the lease.** A deliberate connect in the other app clears its
   `yielded` state: a person asking for the radio is a decision, and a stored flag must not override
   one.

The manifest consequence: both apps declare their receivers, because an implicit broadcast no longer
reaches a background app on Android 8+ and the whole point is the case where the other app *is*
backgrounded. Both use `setPackage`, so no other app can observe or spoof the exchange unless it
claims one of those package names.

## What this costs

One broadcast pair per connect, three seconds of waiting in the worst case, and a contract that has to
be implemented twice. The alternative is what was measured: two apps evicting each other every thirty
seconds, and a range test that cannot tell a radio problem from an app problem.

## What this does not fix

A radio left deaf and mute by the firmware defect above is deaf and mute whatever any app does. The
lease reduces how often that can be triggered; it cannot cure it. The diagnostic that separates the two
is the radio's own counters: a companion whose `tx` airtime does not advance while commands are
`Accepted` is not transmitting, and that is a firmware/reflash matter, not a protocol one.
