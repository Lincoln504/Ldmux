# ADR-0013 — Cross-platform seams

**Status:** Accepted

Date: 2026-09-18

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
the rule was not yet true (§4 — both are now done; this sentence originally said
"is not yet true", present tense, which §4 below contradicts).

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
| `BackToLauncher` | Android has a system back gesture, iOS does not. The *meaning* ("back leaves the shell and lets go") is shared and is the same call the launcher button makes. |
| `rememberTerminalSurface` | The surface is a native view holding a native engine. The tap policy and grid placement above it are shared (`TapPolicy`, `GridPlacement`). **Not yet shared** (found 2026-09-27): the gesture wiring (tap, long-press selection, pinch, drag-to-scroll) and the grid computation still live in `TerminalSurface.android.kt`, and are the extraction to make before iOS, or an iOS port would copy that file. |
| `TerminalEngine` | The terminal emulator is a C library reached through a platform binding. |
| `TerminalPainter` | Text rasterisation: `android.graphics.Paint` on Android, Skia on iOS. |
| `BackgroundSession` | Background policy is the OS's to define. Android may hold a process and a link, iOS suspends within seconds and kills a task that outlives the background window. |
| `CodeScanner` | Reading a QR code needs the camera, which is the OS's: Google's code scanner on Android, VisionKit on iOS. The seam returns text; what the text means (`parsePairingLink`, `scanOutcome`) is shared (ADR-0017). |
| `ScreenOrientation` | Applying an orientation policy is the OS's (an Activity's requested orientation, a scene's supported orientations). Which screen holds which policy is shared (ADR-0011 amendment). |
| `AppPrefs` | Persistent storage is per-platform, and on iOS it is not SharedPreferences. |
| `newDeviceId` / `deviceLabel` | Device identity and the human name of a device come from the OS. |

Each of these is a leaf. None of them decides anything the other platform would
want to decide differently.

`PairingInbox` is deliberately **not** a seam: only the delivery of a link differs
(Android's VIEW intent, iOS's `onOpenURL`), and delivery is one call to `offer`. The
inbox and everything it decides are shared.

## 3. The shape of a good seam

Three tests, in order:

1. **Could the decision be made once?** Then it is not a seam. Move it to
   `commonMain`.
2. **Does the declaration describe the shared concept, or the first platform's
   API?** If the latter, reshape it before the second platform exists, because
   reshaping it afterwards means touching both.
3. **Does the platform half decide anything?** If it does, the decision is in the
   wrong place. A platform half should be able to be a five-line delegation.

## 4. Where the rule was not yet true — both done

This section used to open "Two places. Both are real, both are known, neither is
done." Both are done. It is kept in this form because an ADR that lists outstanding
work and is then quietly edited reads as though the work was never there, and the next
person then derives it again.

### 4.1 The native shim is a portable core, not a JNI file — DONE

`packages/mobile/native/` is three files plus a build file: `ldmux_terminal.h`,
`ldmux_terminal.c` (portable, **no `jni.h`**), `ldmux_jni.c`, `CMakeLists.txt`. The
encoder was extracted and the JNI entry points moved out. Measured after the split: 7
JNI symbols, 7 core symbols, and **0 JNI references inside `ldmux_terminal_render`** —
which is the check this section asked for, since the wire format did not change and the
Android renderer still draws.

### 4.2 Voice dictation has a seam — DONE

`commonMain/.../voice/Voice.kt` holds `VoiceState`, `expect object Dictation` and the
banner, and `androidMain/.../voice/Voice.android.kt` is the `actual`, holding only the
recogniser. The seam takes **no `Activity`**: the Android half attaches its own host,
because an `Activity` in the common declaration would be the first platform's API
wearing the shared concept's name, which is the failure this ADR exists to prevent.

The shared half was written but **not committed** in the same change as the Android
half, so for one commit a fresh clone would not have compiled. A guard now requires
every Kotlin source on disk to be tracked.

One correction to the plan as written here: the button was moved into `commonMain`
without the device verification this section said it needed, and it was then verified on
the device by use rather than by test. That is weaker evidence than a test and should be
treated as such.

## Consequences

- Adding iOS is adding `actual`s, not porting features. The list in §2 is the
  whole of what a second platform has to supply.
- A change to anything not in that list is automatically both platforms, and no
  one has to remember to make it twice.
- The two items that were deferred in §4 are now done, and re-checking them found
  that both had been finished for some time while the ADR still presented them as
  outstanding. A stale todo list in a design document is a claim about the code, and
  it was false.
