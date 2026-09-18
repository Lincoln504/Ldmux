# iOS: what exists, what is missing, and how it would be built

Status: **not started.** There is no `iosMain`. This records the scope, the parts that
are already ready, the machinery that exists on this machine, and the traps — so the
work starts from facts rather than discovery.

## Where the app stands

`packages/mobile/composeApp/src` contains `commonMain/`, `androidMain/` and
`androidUnitTest/`. There is no iOS source set and never has been. Everything below
`commonMain` is shared; everything Android-specific sits behind `expect`/`actual`
(ADR-0013).

**Ten `expect` declarations** need an actual. They are not equal in difficulty:

| Declaration | What iOS needs | Difficulty |
|---|---|---|
| `TerminalEngine` | C interop against `ldmux_terminal.c` | **easy** — the C was already split so the core has no JNI in it |
| `Dictation` | `SFSpeechRecognizer` | **easy** — the seam no longer mentions an Android `Activity` |
| `AppPrefs` | `NSUserDefaults` | easy |
| `newDeviceId`, `deviceLabel` | `UIDevice` | easy |
| `BackgroundSession` | already designed as a number, not a call (`releaseGraceMs`) | easy |
| `BackToLauncher` | a swipe gesture; iPhone has no back button | moderate |
| `lockOrientation` | see below — this one is a real seam constraint | **hard** |
| `TerminalPainter` | Core Text instead of Android `Paint` | **the real work** |
| `rememberTerminalSurface` | the surface, once the painter and engine exist | moderate |

Two of these are already shaped correctly *because* of earlier refactors, and that is
worth stating: the C shim split means the terminal core is portable C and only the
binding is platform code, and the dictation seam now says what dictation *is* rather
than how Android does it. Both were done for exactly this.

**`lockOrientation` deserves a warning.** iOS cannot rotate a running app before
iOS 16; below that, orientation is an Info.plist constraint plus asking the user. That
is a case where *the platform cannot express the shared concept* — the one legitimate
reason for a seam to differ (ADR-0013) — and the honest thing is to write that down in
the ADR rather than pretend the seam is symmetric.

## The machine that would build it

A macOS Sequoia 15.7.9 KVM guest exists and is provisioned:

- Reachable over the tailnet at `debian.tail496756.ts.net:2225` (also
  `100.127.119.54:2225`), account `ldeen`, key `~/.ssh/macos_vm`, alias `ssh seqvm`.
- **Xcode 26.1.1 (17B100)** installed — the last x86-capable Xcode that also satisfies
  Apple's "Xcode 26 or later" App Store rule. 26.2+ are Apple-silicon only, so this is
  the ceiling on this hardware, not a preference.
- The Aura development identity is installed and working:
  `Apple Development: Created via API (8Z7QLPPWGT)`, plus five
  `com.jasperdeen.aura.*` development provisioning profiles.
- Flutter 3.47.4, Rust 1.98.1 and 1.97.1 with the iOS targets, `xcodegen`, `xcodes`,
  `mas`. **No Homebrew and no CocoaPods** — and CocoaPods cannot be installed with the
  guest's Ruby 2.6 in reasonable time (measured: it fetches every version of every
  candidate gem). Flutter's Swift Package Manager path avoids needing it.

Two traps, both measured:

- **`codesign` fails over SSH** with `errSecInternalComponent`, even with the GUI
  session logged in, because the SSH security session cannot use the private key. The
  working route is to launch the build *into the Aqua session*:
  `ssh seqvm 'cp ~/build.sh ~/build.command && chmod +x ~/build.command && open ~/build.command'`.
  GUI auto-login is configured (`/etc/kcpassword` plus `autoLoginUser`), which is what
  makes that possible.
- **There is no systemd unit for the VM.** It must be started by hand, and OpenCore's
  picker defaults to "EFI" — "macOS" has to be selected with a keypress. `quit` in the
  QEMU monitor **powers the machine off**; never send it to a live session.

A third, unrelated to macOS: `earlyoom` runs on the host with `-m 10,5`, and a 6 GB
QEMU guest on a 6-core box is a plausible thing for it to kill. That is worth knowing
before blaming QEMU for a guest that vanishes.

## What is missing

**The ldmux repository is not in the guest.** `~/aura-keyboard` and `~/isle-game` were
copied there; ldmux never was. That is the first step, and it matters that the guest
copy is a *copy* — it has no GitHub credentials, so `git pull` inside the guest fails.

## The iPhone

The target device is the iPhone 15 Plus (iOS 26.6, `00008120-000A7D223EF1A01E`) and
the Apple Watch Series 11 (`00008310-001B0D640CD80032`). Both are past the Xcode 16.2
DeviceSupport ceiling, which is the entire reason the Sequoia + Xcode 26.1.1 guest
exists.

Getting it onto the tailnet is plausible — Tailscale has an iOS client, and the phone
would then reach the daemon the same way the Android phone already does, with no
`adb reverse` and no ports. What is **not** established:

- whether the iPhone's UDID is registered on the Apple ID that owns the development
  profiles. A development profile lists the devices it may run on, so if it is not
  registered the app cannot be installed on the phone regardless of any network work.
- whether the guest can see the phone at all over USB — the VM would need USB
  passthrough for the device, and that has not been attempted here.
- whether the Apple Watch target is separately provisioned. It usually is.

## The scope, stated plainly

1. Copy the repo into the guest, build the C core for the iOS targets.
2. Add `iosMain` with the ten actuals above, easiest first.
3. Get a build onto the simulator, where no provisioning is needed at all — that is
   the useful milestone, and it does not depend on the phone.
4. Then the device: confirm the UDID is registered, then a development build over the
   same route the other test devices use.
5. Keep the invariant that made this possible: nothing enters `commonMain` that cannot
   be expressed without a platform, and a seam is only a seam when the platform cannot
   express the shared concept.
