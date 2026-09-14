/**
 * The daemon wire contract: shared request/response types and a typed client.
 *
 * This is the single source of truth for the Unix-socket RPC surface, imported
 * by the daemon (server), the CLI, tests, and the pi extension (client). No
 * build step: consumers import this module directly.
 */

import { call } from "./socket-client.ts";
import { resolveSocketPath } from "./paths.ts";

export const DAEMON_PROTOCOL_VERSION = 1;

export type SessionKind = "shell" | "pi";
export type SessionStatus = "starting" | "running" | "dead";

export interface SessionRecord {
  id: string;
  /** Empty string when the pi session is not terminal-attached (RPC-only). */
  tmuxName: string;
  kind: SessionKind;
  cwd: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  createdBy: string;
  status: SessionStatus;
  /** Present when a pi session registered itself with the daemon. */
  piSessionId?: string;
  exitCode?: number | null;
  exitedAt?: number | null;
}

export interface PaneState {
  cursorX: number;
  cursorY: number;
  alternateOn: boolean;
  paneInMode: boolean;
  mouse: boolean;
  appCursor: boolean;
  bracketedPaste: boolean;
  historySize: number;
  cols: number;
  rows: number;
  dead: boolean;
}

export interface PingResult {
  pong: true;
  version: number;
  tmuxSocket: string;
}

export interface NewParams {
  kind?: SessionKind;
  cwd?: string;
  title?: string;
  command?: string[];
  createdBy?: string;
}

export interface SendParams {
  id: string;
  text?: string;
  keys?: string[];
}

export interface RegisterParams {
  piSessionId: string;
  cwd: string;
  /** tmux session name when terminal-attached; omitted for RPC-only sessions. */
  tmuxName?: string;
  kind?: SessionKind;
  title?: string;
}

export interface SessionEndParams {
  piSessionId: string;
}

/** Method name -> { params, result }. Drives the typed `daemonCall` below. */
export interface DaemonMethods {
  ping: { params: Record<string, never>; result: PingResult };
  list: { params: Record<string, never>; result: { sessions: SessionRecord[] } };
  new: { params: NewParams; result: { session: SessionRecord } };
  send: { params: SendParams; result: { ok: true } };
  capture: { params: { id: string; history?: boolean }; result: { screen: string } };
  snapshot: { params: { id: string }; result: { screen: string; state: PaneState } };
  kill: { params: { id: string }; result: { ok: true; session: SessionRecord } };
  adopt: { params: { tmuxName: string; kind?: SessionKind; title?: string }; result: { session: SessionRecord } };
  register: { params: RegisterParams; result: { session: SessionRecord } };
  session_end: { params: SessionEndParams; result: { ok: true; session?: SessionRecord } };
  remote_info: {
    params: Record<string, never>;
    result: { hostname: string; port: number; path: string; token: string };
  };
}

export type DaemonMethod = keyof DaemonMethods;

/** Typed call against the daemon socket. */
export function daemonCall<M extends DaemonMethod>(
  socketPath: string,
  method: M,
  params: DaemonMethods[M]["params"],
  timeoutMs?: number,
): Promise<DaemonMethods[M]["result"]> {
  return call(socketPath, method, params as Record<string, unknown>, timeoutMs);
}

/** Result of resolving the default daemon socket path. */
export function defaultSocketPath(): string {
  return resolveSocketPath();
}