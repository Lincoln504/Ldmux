/**
 * Remote access token.
 *
 * The daemon exposes a WebSocket on loopback; remote clients reach it through
 * `tailscale serve` (ADR-0005). Access additionally requires a bearer token
 * stored 0600 in the state dir and rotated only on demand.
 */

import { randomBytes, timingSafeEqual } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

export function loadOrCreateToken(stateDir: string, tokenPath?: string): string {
  const path = tokenPath ?? join(stateDir, "token");
  const legacyPath = join(stateDir, "token");
  const existing = readToken(path);
  if (existing) {
    // A copy left at the old place would be adopted again if the real one were
    // deleted to rotate it — silently bringing the old token back.
    if (tokenPath && tokenPath !== legacyPath) rmSync(legacyPath, { force: true });
    return existing;
  }
  // Moving the token out of the state directory must not unpair a phone that
  // was paired against the old location: the first start with a new path adopts
  // the token that is already there rather than minting one.
  const legacy = tokenPath ? readToken(legacyPath) : "";
  const token = legacy || randomBytes(32).toString("hex");
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  writeFileSync(path, token, { mode: 0o600 });
  chmodSync(path, 0o600);
  if (tokenPath && tokenPath !== legacyPath) rmSync(legacyPath, { force: true });
  return token;
}

function readToken(path: string): string {
  if (!existsSync(path)) return "";
  return readFileSync(path, "utf8").trim();
}

/**
 * Constant-time comparison of the secret bytes.
 *
 * The length check is an early exit, which is safe *here*: the token length is
 * fixed and public, so it leaks nothing, and `timingSafeEqual` would otherwise
 * throw on a length mismatch.
 */
export function tokenMatches(expected: string, provided: string): boolean {
  const a = Buffer.from(expected);
  const b = Buffer.from(provided);
  if (a.length !== b.length) return false;
  return timingSafeEqual(a, b);
}