import { test, expect } from "bun:test";
import {
  FrameType, HEADER_SIZE, encodeFrame, encodeJsonFrame, decodeFrames, decodeJson,
} from "../src/frames.ts";

test("round-trips a binary frame", () => {
  const payload = new Uint8Array([1, 2, 3, 255, 0, 128]);
  const encoded = encodeFrame(FrameType.Output, payload);
  expect(encoded.length).toBe(HEADER_SIZE + payload.length);
  const { frames, rest } = decodeFrames(encoded);
  expect(frames.length).toBe(1);
  expect(frames[0].type).toBe(FrameType.Output);
  expect([...frames[0].payload]).toEqual([...payload]);
  expect(rest.length).toBe(0);
});

test("decodes multiple frames and keeps a partial tail", () => {
  const a = encodeFrame(FrameType.Input, new TextEncoder().encode("hello"));
  const b = encodeFrame(FrameType.Ping);
  const blob = new Uint8Array([...a, ...b, 0x99, 0x99]);
  const { frames, rest } = decodeFrames(blob);
  expect(frames.map((f) => f.type)).toEqual([FrameType.Input, FrameType.Ping]);
  expect([...rest]).toEqual([0x99, 0x99]);
});

test("handles an empty payload", () => {
  const { frames } = decodeFrames(encodeFrame(FrameType.Close));
  expect(frames[0].payload.length).toBe(0);
});

test("json frames round-trip", () => {
  const { frames } = decodeFrames(encodeJsonFrame(FrameType.Hello, { type: "hello", protocolVersion: 1 }));
  expect(decodeJson<{ type: string; protocolVersion: number }>(frames[0])).toEqual({ type: "hello", protocolVersion: 1 });
});

test("rejects oversized payloads", () => {
  expect(() => encodeFrame(FrameType.Output, new Uint8Array(17 * 1024 * 1024))).toThrow();
});