import { test, expect } from "../../testing/src/expect.ts";

// The package entry points exist for consumers that want the whole surface in
// one import (each package declares them in package.json). Nothing in the repo
// imports them, so without a test they would be unverified dead weight. This
// lives in the daemon package because it is the only package allowed to depend
// on both barrels (protocol stays dependency-free, per AGENTS.md).
test("the protocol barrel re-exports the shared contract", async () => {
  const barrel = (await import("../../protocol/src/index.ts")) as Record<string, unknown>;
  for (const name of [
    "encodeFrame",
    "decodeFrames",
    "FrameType",
    "HEADER_SIZE",
    "MAX_FRAME_PAYLOAD",
    "PROTOCOL_VERSION",
    "daemonCall",
    "DAEMON_PROTOCOL_VERSION",
    "resolveStateDir",
    "resolveSocketPath",
    "resolveTmuxSocket",
    "call",
  ]) {
    expect(barrel[name], `missing export ${name}`).toBeDefined();
  }
});

test("the daemon barrel re-exports the daemon's public classes", async () => {
  const barrel = (await import("../src/index.ts")) as Record<string, unknown>;
  for (const name of ["LdmuxDaemon", "Registry", "TmuxControlPlane", "TmuxControlClient", "ControlParser", "decodeOctalEscapes"]) {
    expect(barrel[name], `missing export ${name}`).toBeDefined();
  }
});
