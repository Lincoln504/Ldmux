import { test, expect } from "../../testing/src/expect.ts";
import { Ownership, type Device } from "../src/ownership.ts";

/**
 * Ownership rules (ADR-0010). Every test here states a rule that the *failure*
 * of would be a lockout — a session nobody can open — rather than a cosmetic
 * bug, which is why they are written at this level rather than through the
 * socket.
 */

const phone: Device = { id: "pixel-8", name: "Pixel 8", kind: "phone" };
const desktop: Device = { id: "desktop:debian", name: "debian", kind: "desktop" };
const tablet: Device = { id: "tab-s9", name: "Tab S9", kind: "phone" };

test("a device can claim an unclaimed session", () => {
  const o = new Ownership();
  const res = o.claim("s1", phone, { now: 1000 });
  expect(res.ok).toBe(true);
  expect(o.owner("s1")?.device.id).toBe("pixel-8");
  expect(o.owner("s1")?.since).toBe(1000);
});

test("the holder re-claiming is idempotent, not a conflict", () => {
  // A client re-attaches, resizes and reconnects constantly. Each of those is
  // the holder asking for what it already has; treating it as a conflict would
  // lock a device out of its own session on its second attach.
  const o = new Ownership();
  o.claim("s1", phone, { now: 1000 });
  const again = o.claim("s1", phone, { now: 5000 });
  expect(again.ok).toBe(true);
  expect(o.owner("s1")?.since).toBe(1000); // first claim wins, not last touch
});

test("a re-claim refreshes the device name without moving `since`", () => {
  const o = new Ownership();
  o.claim("s1", phone, { now: 1000 });
  o.claim("s1", { ...phone, name: "Pixel 8 Pro" }, { now: 2000 });
  expect(o.owner("s1")?.device.name).toBe("Pixel 8 Pro");
  expect(o.owner("s1")?.since).toBe(1000);
});

test("a second device is refused, and told who holds it", () => {
  const o = new Ownership();
  o.claim("s1", phone);
  const res = o.claim("s1", desktop);
  expect(res.ok).toBe(false);
  if (res.ok) throw new Error("unreachable");
  expect(res.heldBy.name).toBe("Pixel 8");
  expect(o.owner("s1")?.device.id).toBe("pixel-8"); // unchanged
});

test("takeover moves the claim and reports who it displaced", () => {
  const o = new Ownership();
  o.claim("s1", phone, { now: 1000 });
  const res = o.claim("s1", desktop, { takeover: true, now: 2000 });
  expect(res.ok).toBe(true);
  if (!res.ok) throw new Error("unreachable");
  expect(res.previous?.name).toBe("Pixel 8");
  expect(o.owner("s1")?.device.id).toBe("desktop:debian");
  expect(o.owner("s1")?.since).toBe(2000);
});

test("takeover of an unclaimed session is not an error", () => {
  // The phone always sends takeover:true after its dialog, and the desktop may
  // have gone away in the meantime. That is success, not a failure.
  const o = new Ownership();
  const res = o.claim("s1", desktop, { takeover: true });
  expect(res.ok).toBe(true);
  if (!res.ok) throw new Error("unreachable");
  expect(res.previous).toBeUndefined();
});

test("a displaced device cannot release the session from under the new holder", () => {
  // This is the important one. After a takeover the old device still finishes
  // its in-flight work and sends a release; if that release were honoured the
  // session would end up owned by nobody while someone is actively using it.
  const o = new Ownership();
  o.claim("s1", phone);
  o.claim("s1", desktop, { takeover: true });
  expect(o.release("s1", phone.id)).toBe(false);
  expect(o.owner("s1")?.device.id).toBe("desktop:debian");

  // ...while the real holder's release does free it.
  expect(o.release("s1", desktop.id)).toBe(true);
  expect(o.owner("s1")).toBeUndefined();
});

test("releasing an unclaimed session is a no-op, not an error", () => {
  const o = new Ownership();
  expect(o.release("nope", phone.id)).toBe(false);
});

test("a device may hold several sessions at once", () => {
  const o = new Ownership();
  o.claim("s1", phone);
  o.claim("s2", phone);
  o.claim("s3", desktop);
  expect(o.owner("s1")?.device.id).toBe("pixel-8");
  expect(o.owner("s2")?.device.id).toBe("pixel-8");
  expect(o.owner("s3")?.device.id).toBe("desktop:debian");
});

test("a disconnect frees every session that device held, and only those", () => {
  // The Stale State fix: a claim outliving its connection blocks the next
  // attach, so a socket close has to release everything it was holding.
  const o = new Ownership();
  o.claim("s1", phone);
  o.claim("s2", phone);
  o.claim("s3", desktop);

  const freed = o.releaseDevice(phone.id).sort();
  expect(freed).toEqual(["s1", "s2"]);
  expect(o.owner("s1")).toBeUndefined();
  expect(o.owner("s2")).toBeUndefined();
  expect(o.owner("s3")?.device.id).toBe("desktop:debian"); // untouched

  // Freed sessions are immediately claimable by anyone, including the device
  // that just left — a reconnect must not be refused by its own stale claim.
  expect(o.claim("s1", tablet).ok).toBe(true);
});

test("releasing a device that holds nothing is empty, not a throw", () => {
  const o = new Ownership();
  expect(o.releaseDevice("never-connected")).toEqual([]);
});

test("retain drops claims for sessions that no longer exist", () => {
  const o = new Ownership();
  o.claim("s1", phone);
  o.claim("s2", desktop);
  const dropped = o.retain(new Set(["s1"]));
  expect(dropped).toEqual(["s2"]);
  expect(o.owner("s1")?.device.id).toBe("pixel-8");
  expect(o.owner("s2")).toBeUndefined();
});

test("retain does not leave a dangling reverse index", () => {
  // If `retain` removed the claim but not the device's entry, the device's next
  // disconnect would report freeing a session it no longer holds — and a later
  // session reusing that id would be silently released by it.
  const o = new Ownership();
  o.claim("s1", desktop);
  o.retain(new Set());
  expect(o.releaseDevice(desktop.id)).toEqual([]);
  // The id is reusable and a fresh claim is not considered a conflict.
  expect(o.claim("s1", phone).ok).toBe(true);
});

test("sessionIds reports exactly the claimed sessions", () => {
  const o = new Ownership();
  o.claim("s1", phone);
  o.claim("s2", desktop);
  expect(o.sessionIds().sort()).toEqual(["s1", "s2"]);
  o.release("s1", phone.id);
  expect(o.sessionIds()).toEqual(["s2"]);
});

test("a session id reused after a kill is not inherited by the old holder", () => {
  const o = new Ownership();
  o.claim("s1", phone);
  // s1 dies and is retained away, then a new session happens to reuse the id.
  o.retain(new Set());
  expect(o.owner("s1")).toBeUndefined();
  expect(o.claim("s1", desktop).ok).toBe(true);
});
