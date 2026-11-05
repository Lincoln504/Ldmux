# The host radio: from a board in the box to a phone that can reach a shell

Written 2026-11-03. This is the one page that describes how the *host* side of the LoRa route is
brought up today, what it depends on, and what is still the operator's job. The measurements and
the incident log stay in `docs/HARDWARE-VALIDATION.md`; the protocol stays in
`docs/MESH-STANDALONE.md`; the gateway's own contract stays in `docs/MESH-GATEWAY.md`. Nothing
here is automated yet, and that is the honest headline: there is no installer, no provisioning
screen and no packaging.

## What the pieces are

| Piece | Lives in | Its job |
|---|---|---|
| Firmware | MeshCore (a fork with the USB-serial companion environment) | the radio's own stack: MeshCore routing, the companion protocol, the LoRa preset |
| Serial carrier | `crates/companion` (in this repository) | owns the USB serial port, speaks the companion protocol, hands records to the daemon — **selected by `LDMUX_MESH_SERIAL`** |
| Gateway (alternative) | MeshMore `packages/meshcore_host` (Dart) — **a different repository** | owns the same port and exposes the L2 contract on loopback; still supported for one release, and no longer required |
| Daemon | `crates/daemon` (`LDMUX_MESH_*`) | runs the L3 session on whichever carrier, carries the terminal |
| Phone | `packages/mobile` | owns the other radio over BLE, runs the same L3 session through `libldmux_session.so` |

With `LDMUX_MESH_SERIAL=/dev/serial/by-id/...` the daemon drives the radio itself and **no gateway
runs at all** (2026-11-05). Without it, the gateway path is unchanged: the daemon dials loopback and
learns nothing about presets, paths or contacts. The serial path reads one thing from the radio —
the preset it reports — and derives its airtime budget from it.

## The steps, in the order that works

1. **Pick the two boards.** One is the host radio (USB serial), one is the phone's (BLE). Both
   need MeshCore firmware built with the matching companion environment:
   `heltec_v4_companion_radio_usb` and `heltec_v4_companion_radio_ble` for the HELTEC boards used
   here. The vendor pin in the mobile tree is `companion-v1.15.0`
   (`packages/mobile/vendor/libmeshcore/README.md`), while the firmware actually on the radios for
   the hardware runs was 1.17.1 — the code copes with both, and the pin states provenance rather
   than the only version that works.
2. **Flash the host board, and prove it runs.** `pio run -e heltec_v4_companion_radio_usb -t upload`
   then a hard reset (`esptool.py --after hard_reset chip_id`). "Hard resetting via RTS pin" is
   not proof; the image was observed once not to start after a successful upload
   (`docs/HARDWARE-VALIDATION.md`).
3. **Read the host radio's public key.** `dart run tool/hardware_smoke.dart /dev/ttyACM0` in
   `meshcore_host` prints it (64 hex). That is `LDMUX_MESH_SELF`. It can also come from the
   gateway's state file as `radioKey`, which is where it usually arrives.
4. **Start the gateway.** `dart run bin/meshmore_gateway.dart --port /dev/ttyACM0 --listen 41731
   --token <32 hex> --state <file>`. It writes the state file with `gatewayAddr`, `pairingToken`,
   `peerKey` and `radioKey`; the daemon reads that file when the environment does not supply them.
   `setsid` is needed — plain `nohup` was killed with exit 143 in an earlier session.
5. **Configure the daemon.** `deploy/ldmux.env.example` now carries the block: `LDMUX_MESH_ADDR`,
   `LDMUX_MESH_TOKEN`, `LDMUX_MESH_PEER`, then `LDMUX_MESH_PSK` and `LDMUX_MESH_SELF`. All of the
   first three or none: a partial set is a startup error rather than a daemon that looks configured
   and is not.
6. **Check it with `ldmux mesh status`.** `radio` must print a key. Without one the phone cannot
   address anything, and a pairing code printed now is not usable.
7. **Print the code.** `ldmux code --mesh` (QR) or `ldmux pair --mesh` (link), then scan it in
   Settings on the phone. **Printing a code clears the pinned phone key** — it is a grant, not a
   read. The phone then enrols, and the host pins its key on the first accepted handshake.

## What each failure means

| Symptom | Cause |
|---|---|
| `mesh: no link configured on this host` | no `LDMUX_MESH_*`; the daemon is up and the route is off |
| `state down`, `last bind refused with code 2` | `no_path`: the peer radio is not a contact, or its path is flood-only. The phone bootstraps zero-hop, so this is about the host's own routing table |
| `session off (set LDMUX_MESH_PSK ...)` | no pre-shared key; the L3 session cannot start |
| phone says `the host did not answer` and the host's `mesh status` shows `handshakes` refused | the host does not know this phone: `ldmux code --mesh` on the host re-grants it |
| phone says `the code has no host radio key` | `LDMUX_MESH_SELF` (or the gateway's `radioKey`) is missing; re-scanning the same code cannot fix it |
| the shell freezes and then returns to the shell list | the link died; the client reports it and does not sit on a dead shell |

## What is still the operator's job

- Every step above. There is no provisioning UI, no unit file for the gateway, no packaging of the
  gateway binary and no version handshake between gateway and daemon.
- Knowing which gateway revision and which firmware revision are on the boards. Nothing pins them
  for a user; `docs/HARDWARE-VALIDATION.md` records what was tested.
- Restoring the BLE image on a board that was flashed for USB, and the reverse.

The smallest useful next step is not a UI: it is a `ldmux mesh gateway --port <dev>` that starts
the gateway, reads its state file and writes the daemon's own configuration, so step 4 and step 5
stop being two processes and five environment variables a person has to get right.
