// The packaged native library must match the sources it was built from.
//
// Gradle does not compile C. `libldmux_terminal.so` is produced by
// `scripts/build-libghostty-vt.sh` and Gradle only packages whatever file is
// already sitting in `jniLibs/` — so `gradle :composeApp:assembleDebug` on its own
// ships the last `.so` that was built, however old that is. That is not a
// hypothetical: it produced a shipped APK whose library was a day stale, and the
// only visible symptom was a program's copy going nowhere, which the device
// reports as silence rather than as an error.
//
// Two checks, because staleness and mismatch are different failures:
//
//   1. freshness — a library older than any source it was built from is stale.
//   2. symbols   — every native method Kotlin declares must be *defined* in the
//                  library that will ship. A stale library usually still loads,
//                  and then fails at the first call with UnsatisfiedLinkError,
//                  which is exactly the case a timestamp check alone can miss:
//                  a rebuild that silently failed to pick up a new source file
//                  leaves a fresh timestamp on a library with no new symbol.
//
// Skips cleanly when the library has not been built (a clone has no `.so`, since
// it is a derived artifact and is not committed).
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const NATIVE = join(ROOT, "native");
const JNI_LIBS = join(ROOT, "composeApp", "src", "androidMain", "jniLibs");

/** Every source the shim and the JNI glue are compiled from. */
function nativeSources(): string[] {
  if (!existsSync(NATIVE)) return [];
  return readdirSync(NATIVE)
    .filter((f) => f.endsWith(".c") || f.endsWith(".h"))
    .map((f) => join(NATIVE, f));
}

/** The libraries Gradle will package, one per ABI. */
function packagedLibs(): string[] {
  if (!existsSync(JNI_LIBS)) return [];
  const out: string[] = [];
  for (const abi of readdirSync(JNI_LIBS)) {
    const so = join(JNI_LIBS, abi, "libldmux_terminal.so");
    if (existsSync(so)) out.push(so);
  }
  return out;
}

const SOURCES = nativeSources();
const LIBS = packagedLibs();

test("every packaged .so was built from the current shim sources", () => {
  if (LIBS.length === 0) return; // nothing built yet; nothing to be stale

  const newest = SOURCES.map((s) => ({ s, t: statSync(s).mtimeMs })).sort(
    (a, b) => b.t - a.t,
  )[0];
  assert.ok(newest, "no native sources found, which cannot be right");

  for (const lib of LIBS) {
    const built = statSync(lib).mtimeMs;
    assert.ok(
      built >= newest.t,
      `${lib} is older than ${newest.s}, so it does not contain its current code. ` +
        `Rebuild with: just native-android`,
    );
  }
});

test("every native method Kotlin declares is defined in the packaged .so", () => {
  if (LIBS.length === 0) return;

  const ktPath = join(
    ROOT,
    "composeApp",
    "src",
    "androidMain",
    "kotlin",
    "dev",
    "ldmux",
    "mobile",
    "term",
    "NativeTerminal.kt",
  );
  assert.ok(existsSync(ktPath), "NativeTerminal.kt is where the native methods live");
  const kt = readFileSync(ktPath, "utf8");

  // `external fun takeClipboard(handle: Long): ByteArray?` -> takeClipboard
  const declared = [...kt.matchAll(/\bexternal\s+fun\s+(\w+)\s*\(/g)].map((m) => m[1]);
  assert.ok(declared.length > 0, "no external funs found, so this check is blind");

  for (const lib of LIBS) {
    const defined = new Set(
      execFileSync("nm", ["-D", "--defined-only", lib], { encoding: "utf8" })
        .split("\n")
        .map((l) => l.trim().split(/\s+/)[2])
        .filter(Boolean),
    );
    for (const name of declared) {
      // The JNI mangling for dev.ldmux.mobile.term.NativeTerminal.<name>.
      const symbol = `Java_dev_ldmux_mobile_term_NativeTerminal_${name}`;
      assert.ok(
        defined.has(symbol),
        `${symbol} is declared by Kotlin but not defined in ${lib}. ` +
          `The library is stale or the JNI glue did not compile: just native-android`,
      );
    }
  }
});
