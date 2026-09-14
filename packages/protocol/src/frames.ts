/**
 * ldmux wire framing.
 *
 * Terminal data is binary; control/handshake messages are JSON text. Every
 * frame shares one canonical 8-byte header — there is exactly ONE frame format
 * (dooboxes shipped three incompatible ones; we do not repeat that).
 *
 *   offset  size  field
 *   0       1     type   (FrameType)
 *   1       1     flags  (must be 0: no flags are defined in v1)
 *   2       2     reserved (must be 0)
 *   4       4     length (u32 big-endian, payload byte count)
 *   8       N     payload
 *
 * Only `FrameType.Output` is emitted by the shipped code; the remaining members
 * are the reserved vocabulary for later revisions (see docs/REMOTE.md).
 */

export enum FrameType {
  Hello = 0x01,
  Caps = 0x02,
  Output = 0x10,
  Input = 0x11,
  Resize = 0x12,
  Snapshot = 0x13,
  Follow = 0x14,
  SessionState = 0x15,
  Scrollback = 0x16,
  Ping = 0x30,
  Pong = 0x31,
  Close = 0x40,
  Error = 0xf0,
}

export const HEADER_SIZE = 8;

/** Absolute payload ceiling (DoS guard). */
export const MAX_FRAME_PAYLOAD = 16 * 1024 * 1024;

export interface Frame {
  type: FrameType;
  flags: number;
  payload: Uint8Array;
}

export function encodeFrame(type: FrameType, payload: Uint8Array = new Uint8Array(0), flags = 0): Uint8Array {
  if (payload.length > MAX_FRAME_PAYLOAD) throw new RangeError(`frame payload too large: ${payload.length}`);
  const buf = new Uint8Array(HEADER_SIZE + payload.length);
  const view = new DataView(buf.buffer);
  view.setUint8(0, type);
  view.setUint8(1, flags);
  view.setUint16(2, 0);
  view.setUint32(4, payload.length);
  buf.set(payload, HEADER_SIZE);
  return buf;
}

export function encodeJsonFrame(type: FrameType, value: unknown, flags = 0): Uint8Array {
  return encodeFrame(type, new TextEncoder().encode(JSON.stringify(value)), flags);
}

export interface DecodeResult {
  frames: Frame[];
  rest: Uint8Array;
}

/** Decode as many complete frames as `buf` contains; return the trailing partial. */
export function decodeFrames(buf: Uint8Array): DecodeResult {
  const frames: Frame[] = [];
  let offset = 0;
  while (buf.length - offset >= HEADER_SIZE) {
    const view = new DataView(buf.buffer, buf.byteOffset + offset, HEADER_SIZE);
    const type = view.getUint8(0) as FrameType;
    const flags = view.getUint8(1);
    const length = view.getUint32(4);
    if (length > MAX_FRAME_PAYLOAD) throw new RangeError(`frame payload too large: ${length}`);
    if (buf.length - offset - HEADER_SIZE < length) break;
    const start = offset + HEADER_SIZE;
    const payload = buf.subarray(start, start + length);
    frames.push({ type, flags, payload });
    offset = start + length;
  }
  return { frames, rest: buf.subarray(offset) };
}

export function decodeJson<T = unknown>(frame: Frame): T {
  return JSON.parse(new TextDecoder().decode(frame.payload)) as T;
}