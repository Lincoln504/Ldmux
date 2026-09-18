import { test } from "node:test";
import assert from "node:assert/strict";
import { homedir } from "node:os";
import { join } from "node:path";
import { resolveCwd } from "../src/cwd.ts";

const home = homedir();

// `~` is how every person writes their home directory, and the daemon used to answer
// `not a directory: ~`. These are the cases that decide whether expanding it is safe,
// and each one is a claim about what a path means rather than about what it contains.
test("a bare tilde is the home directory", () => {
  assert.equal(resolveCwd("~"), home);
});

test("a tilde and a path are joined onto home", () => {
  assert.equal(resolveCwd("~/Documents"), join(home, "Documents"));
  assert.equal(resolveCwd("~/a/b/c"), join(home, "a/b/c"));
});

test("a tilde with a slash and nothing after it is still home", () => {
  // `~/` is what a phone keyboard produces from the autocomplete, and it means home
  // rather than a directory named the empty string.
  assert.equal(resolveCwd("~/"), home);
});

// The one that would make the expansion unsafe. `~foo` is a legitimate relative
// directory name -- some shells even create one -- and expanding a leading tilde
// without checking the separator would silently send the shell to `home/foo`.
test("a tilde that is not the whole path and not followed by a separator is left alone", () => {
  assert.equal(resolveCwd("~foo"), "~foo");
  assert.equal(resolveCwd("~foo/bar"), "~foo/bar");
  assert.equal(resolveCwd("a/~/b"), "a/~/b");
});

test("an absolute path is returned unchanged", () => {
  assert.equal(resolveCwd("/tmp"), "/tmp");
  assert.equal(resolveCwd("/home/someone/../someone/x"), "/home/someone/../someone/x");
});

// An absent directory used to mean the daemon's own working directory, which the
// person asking cannot see and did not choose. A home directory is at least a
// directory somebody can name.
test("an absent, blank or non-string directory means home rather than the daemon's cwd", () => {
  assert.equal(resolveCwd(undefined), home);
  assert.equal(resolveCwd(null), home);
  assert.equal(resolveCwd(""), home);
  assert.equal(resolveCwd("   "), home);
  assert.equal(resolveCwd(42), home);
});

test("surrounding whitespace is not part of a path", () => {
  assert.equal(resolveCwd("  /tmp  "), "/tmp");
  assert.equal(resolveCwd(" ~ "), home);
});

// The expansion is arithmetic on a string, and the two ways to get that wrong are to
// pass it through a shell or to reach for a regex. Neither is used, and this is the
// test that would fail first if either crept in.
test("nothing in the path is interpreted beyond the leading tilde", () => {
  assert.equal(resolveCwd("~/$(whoami)"), join(home, "$(whoami)"));
  assert.equal(resolveCwd("~/*"), join(home, "*"));
  assert.equal(resolveCwd("/tmp/../etc"), "/tmp/../etc");
});
