# ADR-0013 — Cross-platform seams

Date: 2026-09-18
Status: Accepted (two refactors deferred, see §4)

## Context

ldmux ships Android first and iOS later. That is a schedule, not an
architecture: an Android-first app that grows a second platform by copying the
first is two apps, and every fix then has to be made twice or is made once and
silently lost on the other side.

The rule that prevents this is that **the shared core is the product and platform
code is a leaf**. Anything that decides *what* happens belongs in `commonMain`.
Anything that only knows *how* to do it on one OS belongs in a platform source
set. A seam is justified only when a platform cannot express the shared concept —
not because a platform has a nicer API for it.

This ADR records that rule, the seams that exist today, and the two places where
the rule is not yet true.

## 1. The rule

- **`commonMain` decides.** Geometry, layout, ownership, protocol, frame
  decoding, pacing, navigation, what a shell is and who may close it. None of
  this is platform-specific, and putting any of it behind a seam is a mistake
  that costs twice.
- **A seam is an `expect`/`actual`, and it needs a reason.** The reason is always
  "this platform cannot express the shared concept", never "this platform does it
  differently". `TerminalPainter` is a seam because text rasterisation is
  genuinely per-platform. `Predictor` is *not* a seam and never will be, because
  prediction is arithmetic.
- **A seam's shape is the shared concept, not the first platform's API.** If the
  declaration reads like the Android API it was written against, the second
  platform has to fight it. `BackgroundSession.releaseGraceMs` is the worked
  example: it started life as an Android capability ("keep the link up") and is
  now the concept both platforms can answer ("how long may this be off screen
  before it must let go"), which is why Android answers 45 s and iOS will answer
  0.
- **Platform code is a leaf.** It may call into the core. The core may not know
  it exists.

## 2. The seams that exist

| Seam | Why it is a seam |
| --- | --- |
| `lockOrientation` | Screen orientation is an OS policy. Android sets it on the Activity, iOS on the scene. The *decision* (one orientation per screen, ADR-0011) is shared. |
| `BackToLauncher` | Android has a system back gesture, iOS does not. The *meaning* ("back leaves the shell and lets go") is shared and is the same call the launcher button makes. |
| `rememberTerminalSurface` | The surface is a native view holding a native engine. Everything above it (sizing, tap policy, gestures, mode) is shared. |
| `TerminalEngine` | The terminal emulator is a C library reached through a platform binding. |
| `TerminalPainter` | Text rasterisation: `android.graphics.Paint` on Android, Skia on iOS. |
| `BackgroundSession` | Background policy is the OS's to define. Android may hold a process and a link, iOS suspends within seconds and kills a task that outlives the background window. |
| `AppPrefs` | Persistent storage is per-platform, and on iOS it is not SharedPreferences. |
| `newDeviceId` / `deviceLabel` | Device identity and the human name of a device come from the OS. |

Each of these is a leaf. None of them decides anything the other platform would
want to decide differently.

## 3. The shape of a good seam

Three tests, in order:

1. **Could the decision be made once?** Then it is not a seam. Move it to
   `commonMain`.
2. **Does the declaration describe the shared concept, or the first platform's
   API?** If the latter, reshape it before the second platform exists, because
   reshaping it afterwards means touching both.
3. **Does the platform half decide anything?** If it does, the decision is in the
   wrong place. A platform half should be able to be a five-line delegation.

## 4. Where the rule is not yet true

Two places. Both are real, both are known, neither is done.

### 4.1 The native shim is a JNI file, not a portable core

`packages/mobile/native/ldmux_terminal.c` holds three things in one file: the
libghostty-vt driving code, **the frame encoder**, and the `JNIEXPORT` entry
points. The encoder is written inline inside `Java_dev_ldmux_mobile_term_NativeTerminal_render`
(`ldmux_terminal.c:205-380`), which means the one piece of logic iOS actually
needs is welded to the one platform Android uses.

This is the only place where "one shared engine" is not yet true, and it is the
cheapest of the remaining work because it is mechanical:

1. Extract the encoder into `size_t ldmux_render_frame(LdmuxTerminal *t, uint8_t **out)`
   returning a byte length, with the `ensure_frame` growth staying inside it.
2. Move the JNI entry points to `ldmux_jni.c`; it allocates a `jbyteArray` and
   copies. Nothing else.
3. Add `ldmux_terminal.h` declaring the core, and list both files in
   `CMakeLists.txt`.

The wire format does not change, so `TerminalFrame.HEADER_SIZE` and the Kotlin
side are untouched, and the check that the work is correct is that the Android
renderer still draws.

### 4.2 Voice dictation has no seam at all

`androidMain/.../voice/VoiceDictation.kt` is Android-only and is called from
`TerminalSurface.android.kt` — an Android file. So the mic button is drawn by
platform code, which means shared code cannot ask for dictation, and iOS would
have to duplicate the button.

This one is not purely mechanical, because the fix is to move the *button* into
`commonMain` and leave only the recogniser behind an `expect`. That is a change
to a composable's source set, so it carries UX risk and needs device
verification, which is why it is not bundled with 4.1.

The seam, when made, is small: `expect object Dictation { fun toggle(onText: (String) -> Unit, onState: (VoiceState) -> Unit); fun stop() }`
with `VoiceState` shared. Android's actual delegates to the existing
implementation unchanged. iOS's is `SFSpeechRecognizer`, and the headless
requirement (dictate without the keyboard appearing) holds on both.

## Consequences

- Adding iOS is adding `actual`s, not porting features. The list in §2 is the
  whole of what a second platform has to supply.
- A change to anything not in that list is automatically both platforms, and no
  one has to remember to make it twice.
- The two deferred items are the exceptions, and they are recorded here so that
  "Android-first" cannot quietly become "Android-shaped".
