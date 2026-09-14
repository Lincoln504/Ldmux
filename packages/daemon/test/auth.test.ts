import { test, expect, beforeAll, afterAll } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadOrCreateToken, tokenMatches } from "../src/auth.ts";

const dir = mkdtempSync(join(tmpdir(), "ldmux-auth-"));
afterAll(() => rmSync(dir, { recursive: true, force: true }));

test("a token is 32 random bytes in hex, stored owner-only", () => {
  const stateDir = mkdtempSync(join(dir, "s1-"));
  const token = loadOrCreateToken(stateDir);
  expect(token).toMatch(/^[0-9a-f]{64}$/);
  expect(readFileSync(join(stateDir, "token"), "utf8")).toBe(token);
  // 0600: the token is a credential even though it lives in a 0700 dir.
  expect(statSync(join(stateDir, "token")).mode & 0o777).toBe(0o600);
});

test("the token is stable across loads (no silent rotation)", () => {
  const stateDir = mkdtempSync(join(dir, "s2-"));
  const first = loadOrCreateToken(stateDir);
  expect(loadOrCreateToken(stateDir)).toBe(first);
  expect(loadOrCreateToken(stateDir)).toBe(first);
});

test("two daemons get different tokens", () => {
  const a = loadOrCreateToken(mkdtempSync(join(dir, "s3-")));
  const b = loadOrCreateToken(mkdtempSync(join(dir, "s4-")));
  expect(a).not.toBe(b);
});

test("an existing token file is adopted rather than overwritten", () => {
  const stateDir = mkdtempSync(join(dir, "s5-"));
  writeFileSync(join(stateDir, "token"), "hand-written-token\n");
  expect(loadOrCreateToken(stateDir)).toBe("hand-written-token");
});

test("an empty or whitespace token file is replaced", () => {
  const stateDir = mkdtempSync(join(dir, "s6-"));
  writeFileSync(join(stateDir, "token"), "   \n");
  const token = loadOrCreateToken(stateDir);
  expect(token).toMatch(/^[0-9a-f]{64}$/);
});

test("token comparison is exact: length, case and content all matter", () => {
  const token = "a".repeat(64);
  expect(tokenMatches(token, token)).toBe(true);
  expect(tokenMatches(token, "a".repeat(63))).toBe(false);
  expect(tokenMatches(token, "a".repeat(65))).toBe(false);
  expect(tokenMatches(token, "A".repeat(64))).toBe(false);
  expect(tokenMatches(token, "b" + "a".repeat(63))).toBe(false);
  expect(tokenMatches(token, "a".repeat(63) + "b")).toBe(false);
  expect(tokenMatches(token, "")).toBe(false);
  expect(tokenMatches("", "")).toBe(true);
  // Non-ASCII/UTF-8 must not sneak through a byte-length comparison.
  expect(tokenMatches("é".repeat(32), "é".repeat(32))).toBe(true);
  expect(tokenMatches("é".repeat(32), "e".repeat(32))).toBe(false);
});

test("comparison does not throw on differently-sized buffers", () => {
  // timingSafeEqual throws on length mismatch; the guard must come first.
  expect(() => tokenMatches("abc", "abcdef")).not.toThrow();
  expect(() => tokenMatches("abcdef", "abc")).not.toThrow();
});
