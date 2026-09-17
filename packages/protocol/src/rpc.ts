/**
 * The daemon wire contract: shared request/response types and a typed client.
 *
 * This is the single source of truth for the Unix-socket RPC surface, imported
 * by the daemon (server), the CLI, and tests. No build step: consumers import
 * this module directly.
 */

import { call } from "./socket-client.ts";
import { resolveSocketPath } from "./paths.ts";

export const DAEMON_PROTOCOL_VERSION = 1;

export type SessionKind = "shell" | "pi";
export type SessionStatus = "starting" | "running" | "dead";

export interface SessionRecord {
  id: string;
  /** Empty string when the session is not terminal-attached (RPC-only). */
  tmuxName: string;
  kind: SessionKind;
  cwd: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  createdBy: string;
  status: SessionStatus;
  exitCode?: number | null;
  exitedAt?: number | null;
}

/**
 * The desktop inventory: what the launcher shows *before* a terminal.
 *
 * Defined here rather than in the daemon because the phone renders it — one
 * description of the shape, or the two drift.
 */
export interface DesktopWorkspace {
  /** The WM's own name for it ("1", "web", "code"). */
  name: string;
  /** Numeric part when the name is a plain number, else null. */
  index: number | null;
  focused: boolean;
  visible: boolean;
  windowCount: number;
}

export interface DesktopWindow {
  id: string;
  appClass: string;
  title: string;
  workspace: string;
  pid: number | null;
}

export interface DesktopPiSession {
  pid: number;
  cwd: string;
  sessionId: string | null;
  startedAt: string | null;
  transcript: string | null;
  windowId: string | null;
  workspace: string | null;
  /** What to show in the menu: the directory's basename. */
  label: string;
}

export interface DesktopInventory {
  /** "i3" | "sway" | "niri" | "hyprland" | "ewmh" | "unknown". */
  wm: string;
  workspaces: DesktopWorkspace[];
  windows: DesktopWindow[];
  piSessions: DesktopPiSession[];
  /** Set when windows could not be listed; pi sessions are still reported. */
  detail?: string;
}

export interface PaneState {
  cursorX: number;
  cursorY: number;
  alternateOn: boolean;
  paneInMode: boolean;
  mouse: boolean;
  /**
   * The running program negotiated SGR mouse reporting (1006). Only then can we
   * synthesize a click it will understand; the legacy encodings cap coordinates
   * at 223 and are not worth supporting.
   */
  mouseSgr: boolean;
  appCursor: boolean;
  bracketedPaste: boolean;
  historySize: number;
  cols: number;
  rows: number;
  dead: boolean;
  /**
   * The pane's foreground command name (tmux `pane_current_command`), e.g.
   * `bash`, `vim`, `less`.
   *
   * This exists because the mouse mode is *sticky*: a program that enables
   * reporting and then exits (or is killed) leaves the mode on, so the emulator
   * — and tmux — still report `mouse: true` while a plain shell is what would
   * actually receive the bytes. A shell prints them, which is exactly the
   * `^[[<0;42;8M` garbage a tap produced on device.
   */
  foreground: string;
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
  /**
   * Tie the session's life to its clients: it is destroyed when the last one
   * detaches. This is how a terminal window opening a tmux session makes it
   * appear while the window is open and removes it when it closes.
   */
  ephemeral?: boolean;
}

export interface SendParams {
  id: string;
  text?: string;
  keys?: string[];
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
  /** Drop a record without touching tmux (cleanup for dead/adopted leftovers). */
  forget: { params: { id: string }; result: { ok: true; removed: boolean } };
  adopt: { params: { tmuxName: string; kind?: SessionKind; title?: string }; result: { session: SessionRecord } };
  desktop: { params: Record<string, never>; result: DesktopInventory };
  makeLive: {
    params: { id: string; workspace: string };
    result: { ok: boolean; detail?: string };
  };
  closeLive: { params: { id: string }; result: { ok: boolean; detail?: string } };
  copyMode: { params: { id: string; on: boolean }; result: { ok: true } };
  tmuxAction: {
    params: { id: string; action: string; name?: string };
    result: { ok: true };
  };
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