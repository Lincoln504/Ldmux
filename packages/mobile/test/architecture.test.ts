// Cross-platform architecture, enforced rather than intended.
//
// The app is one shared core with two independent clients (ADR-0013). That rule is
// easy to state and easy to lose: nothing stops a `commonMain` file from importing
// `android.content.Context`, and once one does, the shared half is no longer shared
// and finding out requires attempting an iOS build — which cannot be done on this
// machine at all. So the rule is checked here, on the source, on every test run.
//
// These are deliberately structural. They do not care how a seam is implemented,
// only that the shared side stays expressible without a platform.

import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const SRC = join(ROOT, "composeApp", "src");
const COMMON = join(SRC, "commonMain", "kotlin");
const ANDROID = join(SRC, "androidMain", "kotlin");

function kotlinFiles(dir: string): string[] {
  const out: string[] = [];
  const walk = (d: string): void => {
    let entries: string[];
    try {
      entries = readdirSync(d);
    } catch {
      return;
    }
    for (const e of entries) {
      const p = join(d, e);
      if (statSync(p).isDirectory()) walk(p);
      else if (e.endsWith(".kt")) out.push(p);
    }
  };
  walk(dir);
  return out;
}

/**
 * Compose's own packages are androidx and that is fine — they are the shared UI
 * toolkit, not the platform. Everything else under androidx is Android, including
 * `androidx.activity` and `androidx.core`.
 */
const COMPOSE_PREFIXES = [
  "androidx.compose.",
  "androidx.annotation.",
];

test("commonMain does not import the Android SDK", () => {
  const offenders: string[] = [];
  for (const file of kotlinFiles(COMMON)) {
    const body = readFileSync(file, "utf8");
    for (const line of body.split("\n")) {
      const m = /^import\s+(android|dalvik)\.[\w.]*/.exec(line.trim());
      if (m) offenders.push(`${file.replace(ROOT, "")}: ${line.trim()}`);
    }
  }
  assert.deepEqual(
    offenders,
    [],
    `the shared half must be expressible without a platform, and these reach into the Android SDK:\n${offenders.join("\n")}`,
  );
});

test("commonMain's androidx imports are Compose, not the platform", () => {
  const offenders: string[] = [];
  for (const file of kotlinFiles(COMMON)) {
    const body = readFileSync(file, "utf8");
    for (const line of body.split("\n")) {
      const m = /^import\s+(androidx\.[\w.]*)/.exec(line.trim());
      if (!m) continue;
      const pkg = m[1];
      if (COMPOSE_PREFIXES.some((p) => pkg.startsWith(p))) continue;
      offenders.push(`${file.replace(ROOT, "")}: ${pkg}`);
    }
  }
  assert.deepEqual(
    offenders,
    [],
    `these androidx packages are Android, so a file that imports one cannot be shared:\n${offenders.join("\n")}`,
  );
});

test("every expect in commonMain has an actual in androidMain", () => {
  const expects: Array<{ file: string; kind: string; name: string }> = [];
  for (const file of kotlinFiles(COMMON)) {
    const body = readFileSync(file, "utf8");
    // `expect fun f(`, `expect object O`, `expect class C`, `expect val v:` —
    // the keyword is what matters, and each is declared with one.
    for (const m of body.matchAll(
      /\bexpect\s+(fun|object|class|val|var|interface)\s+([A-Za-z_]\w*)/g,
    )) {
      expects.push({ file: file.replace(ROOT, ""), kind: m[1], name: m[2] });
    }
  }
  assert.ok(expects.length > 0, "no expect declarations found — has the layout moved?");

  const androidBodies = kotlinFiles(ANDROID).map((f) => readFileSync(f, "utf8"));
  const missing = expects.filter(
    (e) =>
      !androidBodies.some((b) =>
        new RegExp(`\\bactual\\s+${e.kind}\\s+${e.name}\\b`).test(b),
      ),
  );
  assert.deepEqual(
    missing.map((m) => `${m.file}: expect ${m.kind} ${m.name}`),
    [],
    "a seam with no Android side will not compile on Android at all",
  );
});

test("commonMain declares no actual", () => {
  const offenders: string[] = [];
  for (const file of kotlinFiles(COMMON)) {
    const body = readFileSync(file, "utf8");
    for (const m of body.matchAll(/\bactual\s+(fun|object|class|val|var)\s+([A-Za-z_]\w*)/g)) {
      offenders.push(`${file.replace(ROOT, "")}: actual ${m[1]} ${m[2]}`);
    }
  }
  assert.deepEqual(
    offenders,
    [],
    "an actual in the shared half means the shared half is not shared",
  );
});

test("no iosMain exists yet, so no expect is silently waiting on it", () => {
  // iOS is deferred by decision, not by accident: the goal is that nothing here
  // would have to change to add it. Until it exists, this asserts it is genuinely
  // absent rather than half-present, so the ten expects above are the whole list.
  let names: string[] = [];
  try {
    names = readdirSync(SRC);
  } catch {
    assert.fail(`no source sets at ${SRC}`);
  }
  assert.deepEqual(
    names.filter((n) => n.startsWith("ios")),
    [],
    "iosMain appeared — the cross-platform guard needs iOS rules now, not later",
  );
});

test("every Kotlin source on disk is tracked by git", () => {
  // A file the build needs and git does not have is invisible until somebody else
  // builds. That happened here: the voice seam's shared half sat untracked through a
  // whole commit, because on this machine it compiled perfectly. Nothing checked for
  // it, so nothing caught it.
  //
  // This covers every source set rather than the two that exist, so a future iosMain
  // is covered the day it appears.
  let repo = "";
  let tracked: string[] = [];
  try {
    repo = execFileSync("git", ["rev-parse", "--show-toplevel"], { encoding: "utf8" }).trim();
    tracked = execFileSync("git", ["ls-files", "-z", "--", "*.kt", "*.kts"], {
      cwd: repo,
      encoding: "utf8",
    })
      .split("\u0000")
      .filter(Boolean);
  } catch {
    // No git, or not a repository: say so rather than passing silently, since a
    // guard that cannot run is not a guard.
    assert.fail("could not read the git index, so untracked sources cannot be checked");
  }
  const trackedSet = new Set(tracked);

  const onDisk: string[] = [];
  let sets: string[] = [];
  try {
    sets = readdirSync(SRC);
  } catch {
    assert.fail(`no source sets at ${SRC}`);
  }
  for (const s of sets) onDisk.push(...kotlinFiles(join(SRC, s)));

  const untracked = onDisk
    .map((f) => relative(repo, f))
    .filter((p) => !trackedSet.has(p));
  assert.deepEqual(
    untracked,
    [],
    `the build needs these and git does not have them, so a clone would not compile:\n${untracked.join("\n")}`,
  );
  assert.ok(trackedSet.size > 0, "git reported no Kotlin files at all");
});

// The frame format and the daemon's pane limits are one number seen from two
// sides, and neither package can see the other's copy. A frame carries a
// selection's rows in ONE BYTE each while its columns already get two, so a pane
// taller than 255 wraps its selection rows modulo 256: row 256 draws as row 0, the
// handles jump to the top of the screen, and the highlighted range is silently
// wrong. Nothing errors, because nothing is inconsistent — the number is simply
// truncated. Checked here rather than in either package, because this is the seam.
test("the daemon's pane ceiling fits in the frame's one-byte selection rows", () => {
  const plane = readFileSync(
    join(ROOT, "..", "..", "crates", "protocol", "src", "lib.rs"),
    "utf8",
  );
  const m = /pub const MAX_PANE_ROWS: u64 = (\d+);/.exec(plane);
  assert.ok(m, "MAX_PANE_ROWS is no longer a literal this test can read");
  const rows = Number(m[1]);
  assert.ok(
    rows <= 255,
    `MAX_PANE_ROWS is ${rows}, and a selection's rows are one byte on the wire ` +
      `(ldmux_terminal.c header bytes 36 and 39), so rows above 255 wrap and move ` +
      `the handles to the top of the screen with no error`,
  );
});
