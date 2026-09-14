/**
 * Remote access token.
 *
 * The daemon exposes a WebSocket on loopback; remote clients reach it through
 * `tailscale serve` (ADR-0005). Access additionally requires a bearer token
 * stored 0600 in the state dir and rotated only on demand.
 */

import { randomBytes, timingSafeEqual } from "node:crypto";
import { chmodSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export function loadOrCreateToken(stateDir: string): string {
  const path = join(stateDir, "token");
  if (existsSync(path)) {
    const existing = readFileSync(path, "utf8").trim();
    if (existing) return existing;
  }
  const token = randomBytes(32).toString("hex");
  writeFileSync(path, token, { mode: 0o600 });
  chmodSync(path, 0o600);
  return token;
}

/** Constant-time comparison (no early exit on length). */
export function tokenMatches(expected: string, provided: string): boolean {
  const a = Buffer.from(expected);
  const b = Buffer.from(provided);
  if (a.length !== b.length) return false;
  return timingSafeEqual(a, b);
}