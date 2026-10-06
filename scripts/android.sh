#!/bin/sh
# The Android/KMP tasks, one entry point (replaces the old `just android*` / `native-*`
# recipes). Re-enters `nix develop .#mobile` itself when not already inside it.
#
#   scripts/android.sh native [abi ...]  cross-build libghostty-vt + the JNI shim (arm64-v8a x86_64)
#   scripts/android.sh build             native, then the debug APK
#   scripts/android.sh install           build, then `adb install -r` on the attached phone
#   scripts/android.sh test              JVM unit tests (the frame decoder is the wire contract)
#   scripts/android.sh native-test       the shim's own host tests (no device, daemon or emulator)
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
  build) native "$@"; gradle_in_mobile :composeApp:assembleDebug ;;
  install)
    native "$@"; gradle_in_mobile :composeApp:assembleDebug
    adb install -r "$mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk" ;;
  test) gradle_in_mobile :composeApp:testDebugUnitTest ;;
  native-test) "$mobile/scripts/test-native.sh" ;;
  *) echo "usage: $0 native|build|install|test|native-test [args]" >&2; exit 2 ;;
esac
