#!/bin/sh
# The Android/KMP tasks, one entry point (replaces the old `just android*` / `native-*`
# recipes). Re-enters `nix develop .#mobile` itself when not already inside it.
#
#   scripts/android.sh native [abi ...]  cross-build libghostty-vt + the JNI shim (arm64-v8a x86_64)
#   scripts/android.sh build             native, then the debug APK
#   scripts/android.sh install           build, then `adb install -r` on the attached phone
#   scripts/android.sh test              JVM unit tests: the app's, and the vendored frame decoder's
#                                        own (the decoder is the wire contract, and its tests are the
#                                        only ones that assert the frame layouts directly)
#   scripts/android.sh release           native, then the signed APK (needs the signing inputs)
#   scripts/android.sh bundle            native, then the signed App Bundle Play wants
#   scripts/android.sh native-test       the shim's own host tests (no device, daemon or emulator)
#   scripts/android.sh lint              Android lint over the app (0 errors is the bar; the release
#                                        verbs do not run it, so it is its own command)
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
if [ -z "${ANDROID_NDK_HOME:-}" ] && [ -z "${LDMUX_IN_MOBILE_SHELL:-}" ]; then
  LDMUX_IN_MOBILE_SHELL=1 exec nix develop "$root#mobile" --command "$0" "$@"
fi

cmd=${1:-build}
[ $# -gt 0 ] && shift
mobile=$root/packages/mobile
native() { "$mobile/scripts/build-libghostty-vt.sh" "$@"; }
gradle_in_mobile() { (cd "$mobile" && ./gradlew "$@"); }

case "$cmd" in
  native) native "$@" ;;
  # Lint is not in the gate because its version-update warnings change with the calendar, not with
  # the code. Its *errors* are real, and the bar is zero: the Bluetooth reads are silenced in place
  # with the reason, which is what this command exists to keep true.
  lint) gradle_in_mobile :composeApp:lintDebug ;;

  build) native "$@"; gradle_in_mobile :composeApp:assembleDebug ;;
  install)
    native "$@"; gradle_in_mobile :composeApp:assembleDebug
    adb install -r "$mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk" ;;
  # **The vendored decoder's own suite runs too.** Its frames are the wire contract — a wrong opcode or
  # a length byte read as a count is a link that never comes up — and until this line existed nothing
  # ran those tests at all. `libmeshcore-android`'s unit tests need an Android toolchain for its test
  # variant and are not part of this gate; the app's own suite covers the same decoders from above.
  test) gradle_in_mobile :libmeshcore:test :composeApp:testDebugUnitTest ;;
  # The only artifacts Play accepts. They fail, loudly and early, when the signing inputs
  # are absent: an unsigned bundle is rejected at upload, which is a worse place to learn it.
  # A release artifact with a failing Kotlin suite is a release nobody checked, and nothing else
  # in the path runs that suite (CI builds Rust only). The test task is cheap next to the build,
  # so it runs first and a red suite stops the artifact before it exists.
  release) native "$@"; gradle_in_mobile :libmeshcore:test :composeApp:testDebugUnitTest; ./scripts/collect-licenses.sh; gradle_in_mobile :composeApp:assembleRelease ;;
  bundle) native "$@"; gradle_in_mobile :libmeshcore:test :composeApp:testDebugUnitTest; ./scripts/collect-licenses.sh; gradle_in_mobile :composeApp:bundleRelease ;;
  native-test) "$mobile/scripts/test-native.sh" ;;
  *) echo "usage: $0 native|build|install|test|release|bundle|native-test [args]" >&2; exit 2 ;;
esac
