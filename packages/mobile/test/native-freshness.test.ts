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
import { createHash } from "node:crypto";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const NATIVE = join(ROOT, "native");
const JNI_LIBS = join(ROOT, "composeApp", "src", "androidMain", "jniLibs");

/** Every source file that lives beside the shim, whether or not it is compiled. */
function nativeSources(): string[] {
  if (!existsSync(NATIVE)) return [];
  return readdirSync(NATIVE)
    .filter((f) => f.endsWith(".c") || f.endsWith(".h"))
    .map((f) => join(NATIVE, f));
}

/**
 * Sources beside the shim that are deliberately **not** in the packaged terminal library.
 *
 * Three reasons, all of them honest ones:
 *
 * - `test_engine.c` is the *host* suite: `scripts/test-native.sh` compiles it against the portable
 *   core and runs it here. It is linked into no Android library anywhere.
 * - `test_jni.c` is the same idea for the shim.
 * - `ldmux_session.h` and `ldmux_session_jni.c` belong to the **second** library,
 *   `libldmux_session.so` (Rust session plus its JNI shim, `native/build-session.sh`), which is
 *   built separately so a session change does not rebuild libghostty (`docs/ARCHITECTURE.md` §3).
 *
 * The stamp used to be a glob of `*.c *.h`, so editing one of these declared the packaged `.so`
 * stale and demanded a full NDK rebuild to clear a warning about a file that is not in the
 * artefact.
 *
 * They stay in this list rather than being ignored, because the rule that matters is the one below
 * it: every source file has to be accounted for. A file that is neither stamped nor named here is a
 * file somebody added and did not classify, and that is exactly when silence is expensive.
 */
const NOT_IN_TERMINAL_LIB = new Set([
  "test_engine.c",
  "test_jni.c",
  "ldmux_session.h",
  "ldmux_session_jni.c",
]);

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

  // Content first. The stamp is what the build actually compiled, so an edit that
  // changes nothing, or a fresh checkout that rewrites every file's timestamp,
  // cannot make a correct library look stale — and a real source change cannot
  // hide behind a timestamp that happens to be older.
  const stampPath = join(NATIVE, "prebuilt", "sources.sha256");
  if (existsSync(stampPath)) {
    // Names are normalised so the comparison is between bytes and not between
    // one side's idea of how to spell a path.
    const recorded = readFileSync(stampPath, "utf8")
      .trim()
      .split("\n")
      .filter(Boolean)
      .map((line) => line.replace(/  \.\//, "  "));
    // Hashed by the name the stamp recorded, not by a walk of the directory: the
    // stamp is the list of build inputs, and asking about anything else is asking
    // about a file that is not in the artefact.
    const current = recorded
      .map((line) => {
        const name = line.slice(line.indexOf("  ") + 2);
        const sum = createHash("sha256")
          .update(readFileSync(join(NATIVE, name)))
          .digest("hex");
        return `${sum}  ${name}`;
      })
      .sort((a, b) => (a.slice(66) < b.slice(66) ? -1 : 1));
    assert.deepEqual(
      current,
      recorded,
      "the packaged .so was built from different shim sources than the ones on disk. " +
        "Rebuild with: nix develop .#mobile -c scripts/build-libghostty-vt.sh",
    );

    // And nothing may be unclassified. A new source file is either compiled into
    // the library — in which case it must be stamped, or a real change to it would
    // ship an unrebuilt .so — or it is not, in which case it belongs in HOST_ONLY.
    const stamped = new Set(recorded.map((l) => l.slice(l.indexOf("  ") + 2)));
    const unclassified = nativeSources()
      .map((s) => basename(s))
      .filter((f) => !stamped.has(f) && !NOT_IN_TERMINAL_LIB.has(f))
      .sort();
    assert.deepEqual(
      unclassified,
      [],
      "a native source is neither in the build stamp nor declared host-only. " +
        "Add it to SHIM_SOURCES in scripts/build-libghostty-vt.sh if the library " +
        "compiles it, or to HOST_ONLY in this test if it does not.",
    );
    return;
  }

  // No stamp (an older tree): fall back to timestamps, which is weaker but still
  // catches the thing this exists for — a library that predates its source.
  const newest = SOURCES.map((s) => ({ s, t: statSync(s).mtimeMs })).sort(
    (a, b) => b.t - a.t,
  )[0];
  assert.ok(newest, "no native sources found, which cannot be right");

  for (const lib of LIBS) {
    const built = statSync(lib).mtimeMs;
    assert.ok(
      built >= newest.t,
      `${lib} is older than ${newest.s}, so it does not contain its current code. ` +
        `Rebuild with: nix develop .#mobile -c scripts/build-libghostty-vt.sh`,
    );
  }
});

// The stamp above covers the shim sources. Two other things go into the build command
// and were recorded nowhere: the commit of the engine the library wraps, and the API
// level it was compiled against. Changing either rebuilds a materially different library
// while leaving the freshness check above perfectly green — which is the one thing this
// file exists to prevent.
test("the packaged .so records the engine pin and API level it was built from", () => {
  if (LIBS.length === 0) return; // nothing built yet; nothing to be wrong

  const inputsPath = join(NATIVE, "prebuilt", "build-inputs.txt");
  assert.ok(
    existsSync(inputsPath),
    "the packaged .so predates the record of what it was built from, so a change to the " +
      "engine pin or the API level would leave it looking fresh. Rebuild: nix develop .#mobile -c scripts/build-libghostty-vt.sh",
  );
  const fields = Object.fromEntries(
    readFileSync(inputsPath, "utf8")
      .trim()
      .split("\n")
      .filter(Boolean)
      .map((line) => [line.slice(0, line.indexOf(" ")), line.slice(line.indexOf(" ") + 1)]),
  );

  // The pin is read out of native/ghostty.pin (the one file the flake and the build script
  // both read) rather than restated here. Restating it is how the two came to disagree:
  // the script would build one engine and this would compare against another, and both
  // would look correct in isolation.
  const pinFile = readFileSync(join(NATIVE, "ghostty.pin"), "utf8");
  const pin = /^commit[ \t]+([0-9a-f]{40})\b/m.exec(pinFile)?.[1];
  assert.ok(pin, "could not read a full 40-char `commit` out of native/ghostty.pin");
  assert.equal(
    fields["engine-commit"],
    pin,
    `the packaged .so wraps engine ${fields["engine-commit"]} but native/ghostty.pin pins ` +
      `${pin}. Rebuild with: nix develop .#mobile -c scripts/build-libghostty-vt.sh`,
  );
  assert.ok(fields["api-level"], "no api-level was recorded by the build");
});

test("every native method Kotlin declares is defined in the packaged .so", () => {
  if (LIBS.length === 0) return;

  const ktPath = join(
    ROOT,
    "composeApp",
    "src",
    "androidMain",
    "kotlin",
    "io",
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
          `The library is stale or the JNI glue did not compile: nix develop .#mobile -c scripts/build-libghostty-vt.sh`,
      );
    }
  }
});
