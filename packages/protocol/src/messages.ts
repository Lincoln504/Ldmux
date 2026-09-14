/**
 * JSON control messages carried as text frames (see frames.ts). Terminal data
 * travels as binary OUTPUT/INPUT frames; these are the control/handshake shapes.
 */

import type { SessionKind, SessionStatus } from "./rpc.ts";

export const PROTOCOL_VERSION = 1;

export interface Hello {
  type: "hello";
  protocolVersion: number;
  clientId?: string;
  resumeToken?: string;
  lastSnapshotEpoch?: number;
}

export interface Caps {
  type: "caps";
  protocolVersion: number;
  sessionId: string;
  cols: number;
  rows: number;
  snapshotEpoch: number;
  capabilities: { controlMode: boolean; scrollback: boolean };
}

/** Snapshot of the visible screen + terminal state (ADR-0004 fidelity fields). */
export interface Snapshot {
  type: "snapshot";
  sessionId: string;
  cols: number;
  rows: number;
  /** Visible screen with SGR escapes preserved (capture-pane -e). */
  screen: string;
  cursorX: number;
  cursorY: number;
  alternateOn: boolean;
  paneInMode: boolean;
  bracketedPaste: boolean;
  mouse: boolean;
  appCursor: boolean;
  historySize: number;
  snapshotEpoch: number;
}

/** Client-owned scroll intent (ADR-0004). The server never resolves line ids. */
export interface Follow {
  type: "follow";
  mode: "following" | "anchored";
}

export interface Ping { type: "ping"; id?: number }
export interface Pong { type: "pong"; id?: number }
export interface ErrorMessage { type: "error"; code: string; message: string }
export interface Close { type: "close"; reason?: string }

/** Remote client → server: subscribe this connection to one session's stream. */
export interface Attach { type: "attach"; sessionId: string }
/** Remote client → server: type literal text into the attached pane. */
export interface Input { type: "input"; data: string }
/** Remote client → server: send named keys (Enter, C-c, Up, ...). */
export interface KeyMessage { type: "key"; keys: string[] }

/** A session as advertised to remote clients (subset of SessionRecord). */
export interface SessionInfo {
  id: string;
  kind: SessionKind;
  title: string;
  cwd: string;
  status: SessionStatus;
  tmuxName: string;
  piSessionId?: string;
}

/** Remote client → server: list sessions. */
export interface ListSessions { type: "list" }
/** Server → client: answer to `list`. */
export interface SessionsMessage { type: "sessions"; sessions: SessionInfo[] }

export type ControlMessage =
  | Hello
  | Caps
  | Snapshot
  | Follow
  | Ping
  | Pong
  | ErrorMessage
  | Close
  | Attach
  | Input
  | KeyMessage
  | ListSessions
  | SessionsMessage;