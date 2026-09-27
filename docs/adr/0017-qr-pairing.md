# ADR-0017 — Pairing is a QR code from `ldmux code`

- **Status:** Accepted
- **Date:** 2026-09-27
- **Supersedes:** the pairing half of ADR-0005
- **Related:** ADR-0016 (direct tailnet bind), ADR-0013 (cross-platform seams)

## Context

A phone was paired by typing the host and a 64-character token into two text
fields in Settings. `ldmux pair` printed four things, and none of them connected
a phone:

- an `https://` URL for the deleted web client;
- a loopback URL;
- a deep link with port 443 for a `tailscale serve` that was not configured;
- and the `serve` command.

The app had no handler for the deep link either.

## Decision

**The link.**

    ldmux://pair?v=1&host=100.127.119.54:7625&token=<64 hex>&name=debian

- `packages/protocol/src/pairing.ts` defines it.
- `host` is the address the daemon listens on, from `remote_info`, never a guess
  about exposure.
- The app refuses a link whose host is not tailnet or loopback
  (`protocol/src/net.ts` and its Kotlin mirror). A crafted code therefore cannot
  point the phone at a machine off the tailnet.
- `v` is the format version, and an unknown version is refused.
- `packages/protocol/test/fixtures/pairing-links.json` pins the format from both
  sides: the TypeScript tests and the app's `PairingTest` read the same cases.

**The computer.**

- `ldmux code` draws the link as a QR code in the terminal.
  - The encoder is `packages/daemon/src/qr.ts`, written here because production
    has one dependency and the pinned runtime has no QR tool.
  - It is judged by decoding, not by comparing bits: zbar, a devShell-only test
    tool, reads back every version at every level, plus the terminal rendering
    itself.
- `ldmux pair` prints the same link as text.
- Both refuse, with the fix, a daemon a phone cannot reach: one on loopback, or
  one not listening yet.
- `ldmux expose` is gone (ADR-0016).

**The phone.**

- **Unpaired is a state, not a screen.** The launcher says `unpaired`, and pairing
  is in Settings. The app never jumps to a scanner by itself.
- **Settings has one pairing row.**
  - Unpaired: `pair`, one tap, opens the scanner. A good scan pairs and closes
    Settings.
  - Paired: the machine's name and `unpair`. Unpair asks once
    (`unpair` → `unpair?`), because it ends every open shell's link.
  - The host and token text fields are gone.
- **The scanner is a seam, `CodeScanner`.** It returns scanned text or nothing,
  and parsing stays in `commonMain`.
  - Android uses Google's code scanner (`play-services-code-scanner`). It needs no
    camera permission and is a full-screen scanner that closes itself on a read.
  - iOS will use VisionKit's `DataScannerViewController` behind the same
    declaration.
- **Deep links go through one inbox, `PairingInbox`, in `commonMain`.** A link
  opened from outside (the system camera, a message) is delivered there by the
  platform: Android's intent, and iOS's `onOpenURL` later. A link from outside
  is *asked about* (`pair with debian?`) before it replaces anything. A scan from
  the app's own button is not, because the button was the question.
- **Unpairing forgets the pairing on this phone.** It does not revoke the token:
  there is one shared token (`auth.ts`), so revoking it would unpair every
  device. Per-device tokens are the follow-up that makes unpair a revocation.

## Consequences

- **+** Pairing is `ldmux code` on the computer and two taps on the phone: settings,
  then `pair`.
- **+** The scanner shows no permission prompt and needs no camera code in the app.
- **−** The Android scanner needs Google Play services. Without them `pair` says
  `scanner unavailable`, and the system camera can still open an `ldmux://`
  link, which arrives through `PairingInbox`.
- **−** Anyone who sees the code on screen has the token. It is the same secret
  the old text fields showed, and the same exposure.
