/**
 * Who is looking at a session (ADR-0010).
 *
 * One device owns a session at a time. The whole reason this is a module and not
 * a field on a record is the failure mode dooboxes' own glossary names *Stale
 * State*: attachment state that outlives the connection **blocks the next
 * attach**. The failure is a lockout, not a cosmetic bug.
 *
 * So ownership is a **map of live claims**, not durable state:
 *   - a device's claims are dropped when its connection ends ([releaseDevice]);
 *   - claims for sessions that no longer exist are dropped by [retain];
 *   - nothing is ever written to disk. A daemon restart loses every claim, which
 *     is correct — a restart also loses every client.
 *
 * Pure and synchronous: every rule here is a rule the tests can state directly.
 */

import type { DeviceInfo } from "../../protocol/src/rpc.ts";

export type DeviceKind = "phone" | "desktop";

/**
 * A device identity, shared with the wire (`DeviceInfo`) so the owner the daemon
 * records and the owner a client is told about cannot drift apart.
 */
export type Device = DeviceInfo;

export interface Claim {
  device: Device;
  /** Epoch ms, for "open on Pixel since 14:02". */
  since: number;
}

export type ClaimResult =
  /** Acquired (or re-acquired by its holder — see [Ownership.claim]). */
  | { ok: true; claim: Claim; previous?: Device }
  /** Someone else holds it. The caller decides whether to ask the user. */
  | { ok: false; heldBy: Device };

/**
 * The claim table.
 *
 * `held` is a reverse index purely so a disconnect is O(sessions-of-that-device)
 * rather than O(all claims). Keeping the two maps in step is the only invariant
 * here, and it is why every mutation goes through a private setter.
 */
export class Ownership {
  readonly #claims = new Map<string, Claim>();
  readonly #held = new Map<string, Set<string>>();

  /** The device currently looking at this session, if any. */
  owner(sessionId: string): Claim | undefined {
    return this.#claims.get(sessionId);
  }

  /** Every claimed session id, for broadcast/retain checks. */
  sessionIds(): string[] {
    return [...this.#claims.keys()];
  }

  /**
   * Claim a session for a device.
   *
   * The same device re-claiming is **idempotent, not a conflict**: a client
   * reconnects, resizes, or re-attaches constantly, and every one of those is the
   * holder asking for what it already has. Only a *different* device is a
   * conflict, and it is never resolved here — `takeover` is the caller saying the
   * user consented, which is the only way a claim changes hands.
   */
  claim(
    sessionId: string,
    device: Device,
    opts: { takeover?: boolean; now?: number } = {},
  ): ClaimResult {
    const now = opts.now ?? Date.now();
    const existing = this.#claims.get(sessionId);

    if (existing && existing.device.id === device.id) {
      // Refresh identity (a name can change) without disturbing `since`: the
      // interesting question is when it was taken, not when it was last touched.
      const refreshed: Claim = { device, since: existing.since };
      this.#claims.set(sessionId, refreshed);
      return { ok: true, claim: refreshed };
    }

    if (existing && !opts.takeover) return { ok: false, heldBy: existing.device };

    if (existing) this.#unindex(sessionId, existing.device.id);
    const claim: Claim = { device, since: now };
    this.#claims.set(sessionId, claim);
    this.#index(sessionId, device.id);
    return { ok: true, claim, previous: existing?.device };
  }

  /**
   * Release one session. Returns whether the caller actually held it.
   *
   * A non-holder releasing is a no-op rather than an error: a device that was
   * already displaced (someone took over) will still send its release, and that
   * must not free the session from under the new holder.
   */
  release(sessionId: string, deviceId: string): boolean {
    const existing = this.#claims.get(sessionId);
    if (!existing || existing.device.id !== deviceId) return false;
    this.#claims.delete(sessionId);
    this.#unindex(sessionId, deviceId);
    return true;
  }

  /**
   * Drop everything a device holds — what a closing socket calls.
   *
   * Returns the freed session ids so the daemon can broadcast exactly the
   * sessions whose owner changed, rather than all of them.
   */
  releaseDevice(deviceId: string): string[] {
    const sessions = [...(this.#held.get(deviceId) ?? [])];
    for (const sessionId of sessions) {
      this.#claims.delete(sessionId);
    }
    this.#held.delete(deviceId);
    return sessions;
  }

  /**
   * Drop claims for sessions that no longer exist.
   *
   * The session-level half of the Stale State problem: a session killed outside
   * ldmux (`exit`, a hand-run `tmux kill-session`) leaves a claim pointing at
   * nothing, and a later session reusing that id would inherit it.
   */
  retain(liveSessionIds: ReadonlySet<string>): string[] {
    const dropped: string[] = [];
    for (const [sessionId, claim] of this.#claims) {
      if (liveSessionIds.has(sessionId)) continue;
      this.#claims.delete(sessionId);
      this.#unindex(sessionId, claim.device.id);
      dropped.push(sessionId);
    }
    return dropped;
  }

  #index(sessionId: string, deviceId: string): void {
    let set = this.#held.get(deviceId);
    if (!set) {
      set = new Set();
      this.#held.set(deviceId, set);
    }
    set.add(sessionId);
  }

  #unindex(sessionId: string, deviceId: string): void {
    const set = this.#held.get(deviceId);
    if (!set) return;
    set.delete(sessionId);
    if (set.size === 0) this.#held.delete(deviceId);
  }
}
