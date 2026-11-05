# Store submission: what Play requires, and the answers

Written 2026-11-05. This is the material for the Play Console forms, kept beside the code
so it is reviewed with it. Nothing here is a promise the code does not keep.

## What the app is, in one sentence

A client for a terminal that runs on a Linux machine you own. It speaks to that machine
over your tailnet, or over a LoRa radio if the phone has one.

That sentence is the short description, the first line of the full description, and the
first line of the listing. It matters because everything the app does depends on a host the
reviewer does not have: an app that does not say so reads as broken at first launch.

## Privacy policy

Play requires a hosted URL for every app, including one that collects nothing. The text
below is the policy; host it and put the URL in the console.

> **ldmux collects nothing.**
>
> The app connects only to a machine you name yourself: your own computer, reached over a
> private tailnet or over a radio link. There is no account, no sign-in, and no server run by
> the developer.
>
> - **No analytics, no crash reporting, no telemetry.** The app contains no such SDK, so
>   there is nothing to report and nothing to opt out of.
> - **No data leaves your device except to your own machine.** What you type is sent to the
>   host you paired with. It is not sent anywhere else.
> - **The pairing token is stored on the device and excluded from backup.** Android's backup
>   and device-transfer rules exclude it, so it cannot be moved off the phone by the system.
> - **The camera is not used.** Pairing scans a QR code through Google's Play services code
>   scanner, which decodes on the device and sends nothing to the developer.
> - **Speech is optional and separate.** Dictation uses Android's own speech recogniser. If
>   the device's recogniser is cloud-based, audio may go to that provider, which is the
>   device's setting and not something this app controls. The app tells you when that is the
>   case.
>
> Questions: open an issue on the project's repository.

## Data safety form

| Question | Answer | Why |
|---|---|---|
| Does your app collect or share any of the required user data types? | **No** | No analytics, no crash reporting, no account, no developer server. |
| Is data encrypted in transit? | Not applicable | The only traffic is to a host the user chose, over a WireGuard tailnet or a radio link; the developer is not a party to it. |
| Can users request data deletion? | Not applicable | Nothing is held by the developer. |

Written down before opening the form, because the form is unforgiving and "no" is only
defensible if it is true of the code. It is: a dependency audit for firebase, crashlytics,
sentry, bugsnag or any analytics SDK finds none.

## Permission justifications

Each one is required by the console, and each answer is already true of the code.

| Permission | Declaration |
|---|---|
| `BLUETOOTH_SCAN` (with `neverForLocation`) | Finds the phone's own MeshCore radio. Never used to derive location, so it is declared with `neverForLocation` and no location access is requested on Android 12 or later. |
| `BLUETOOTH_CONNECT` | Connects to that radio. |
| `BLUETOOTH`, `BLUETOOTH_ADMIN`, `ACCESS_FINE_LOCATION` (`maxSdkVersion=30`) | The same radio on Android 8.0 to 11, where a BLE scan requires location permission. Not requested on newer releases. |
| `RECORD_AUDIO` | Dictation: turning speech into text at the terminal prompt. Requested when the microphone is first used, never at launch. |
| `FOREGROUND_SERVICE` and `FOREGROUND_SERVICE_SPECIAL_USE` | Keeps an interactive terminal connection alive for a 45-second grace period after the user switches away, so a quick app switch does not drop the open shell. The subtype string is in the manifest and is the text to paste. |
| `POST_NOTIFICATIONS` | The service's own notification. Denial is not fatal. |
| `WAKE_LOCK` | Partial, bounded, released with the service. |
| `INTERNET`, `ACCESS_NETWORK_STATE` | The connection to the host. |

## Listing copy

Short description (80 characters):

```
A terminal for a shell on your own Linux machine, over tailnet or radio.
```

Full description:

```
ldmux is a terminal for a shell that runs on a Linux machine you own, not on the phone.

The shell lives on the host, in a keeper process that outlives every client: put the phone
down, lose the network, restart the app, and the shell is still there with its scrollback.
Four things follow from that.

Real programs. It is a terminal, so `vim`, `ssh`, `top` and anything else your shell can run
work, with the alternate screen, mouse reporting, bracketed paste and scrollback intact.

Two ways to reach it. Over your own tailnet, which needs no server of ours because there is
no server of ours, or over a LoRa radio when there is no network at all.

More than one way in. The same shell can be on the phone and in a window on the desktop at
once, and either can take the keyboard.

Nothing collected. No account, no analytics, no crash reporting, no developer server. The
token that pairs the phone with your machine never leaves the device, and is excluded from
backup.

What you need: a Linux machine with a systemd user session, the ldmux daemon installed on it
(`nix run .#install`, or the install script), and Tailscale if you want the network route.
The radio route needs two MeshCore companion radios and no network at all.
```

## Content rating and audience

- No user-generated content is hosted by the developer; there is no social surface.
- No ads, no purchases, no gambling, no violence.
- The app is a development tool. Target audience: adults; no child-directed content.
- The questionnaire answers follow from the above, and the app contains nothing that would
  raise a rating above "Everyone".

## Decisions that must be made before the first upload

- **`applicationId` is `dev.ldmux.mobile`**, which asserts a domain. Play does not verify
  ownership, but publishing under a domain the project does not control is an impersonation
  risk, and the id cannot be changed after the first upload. Confirm ownership of
  `ldmux.dev` before the first bundle, or change the id now.
- **The privacy policy needs a URL.** The text above is ready; hosting it is not.
- **Store assets do not exist yet**: feature graphic (1024x500), phone screenshots, a 512x512
  icon. The launcher icon in `res/` is an in-app icon, not a store asset.

## Cutting a release

1. `packages/mobile/keystore.properties`, or the four environment variables, holding the
   upload key. Never committed; `.gitignore` covers it.
2. `scripts/android.sh bundle` — builds the native session per ABI and the signed AAB.
   Without the signing inputs the task fails by design, rather than producing a bundle Play
   will reject.
3. `versionCode` is the git commit count, so it only rises. `LDMUX_VERSION_CODE` overrides it
   when re-cutting an already-uploaded commit, and `LDMUX_VERSION_NAME` sets the display
   version.
4. Upload to a closed track first, with the phone on the tailnet and on the radio in turn.
   Everything the repository calls "verified on a device" was last exercised on
   `feat/standalone-radio` in November 2026, so the artifact being shipped needs its own pass.
