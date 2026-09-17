/**
 * JSON control messages carried as text frames (see frames.ts). Pane output
 * travels as binary OUTPUT frames; everything in the other direction is one of
 * these JSON shapes.
 */

import type { DesktopInventory, SessionRecord } from "./rpc.ts";

export const PROTOCOL_VERSION = 1;

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
  /** The program negotiated SGR mouse reporting, so a synthesized click is valid. */
  mouseSgr: boolean;
  appCursor: boolean;
  historySize: number;
  snapshotEpoch: number;
  /** True when `screen` includes paged scrollback (answer to `scrollback`). */
  history?: boolean;
}

/** Client-owned scroll intent (ADR-0004). The server never resolves line ids. */
export interface Follow {
  type: "follow";
  mode: "following" | "anchored";
}

export interface Ping { type: "ping"; id?: number }
export interface Pong { type: "pong"; id?: number }
export interface ErrorMessage { type: "error"; code: string; message: string }

/** Remote client → server: subscribe this connection to one session's stream. */
export interface Attach {
  type: "attach";
  sessionId: string;
  /**
   * The grid the client will paint. The viewing client owns the pane geometry,
   * so the daemon resizes tmux to this before snapshotting (a phone's viewport
   * is the authority on its own screen size). Omitted = leave the pane alone.
   */
  cols?: number;
  rows?: number;
}

/**
 * Remote client → server: the user tapped a cell. The server decides what that
 * means, because only it can see the pane:
 *   - a mouse-aware program gets a synthesized SGR click (vim, less, htop, a
 *     TUI) — byte-identical to a real click, injected straight to the pty;
 *   - a plain shell gets the tap turned into cursor motion by the *client*, so
 *     this message is not sent at all.
 * Coordinates are 0-based cells.
 */
export interface MouseClick { type: "mouseclick"; col: number; row: number }

/**
 * Remote client → server: the viewport changed. Like `attach`'s cols/rows, this
 * is the client stating its own geometry; the daemon applies it to tmux and
 * answers with a fresh snapshot so the repaint matches.
 */
export interface Resize { type: "resize"; cols: number; rows: number }
/** Remote client → server: type literal text into the attached pane. */
export interface Input { type: "input"; data: string }
/** Remote client → server: send named keys (Enter, C-c, Up, ...). */
export interface KeyMessage { type: "key"; keys: string[] }

/**
 * Enter or leave tmux copy-mode for a session.
 *
 * Needed because scrollback only exists for the *normal* screen. A full-screen
 * program draws on the alternate screen, which has no history to keep, so
 * scrolling it means asking tmux to scroll its own view of the pane.
 */
export interface CopyModeMessage {
  type: "copymode";
  sessionId?: string;
  on: boolean;
}

/**
 * A tmux prefix-key action.
 *
 * The prefix key cannot be *sent* — `send-keys C-b` types 0x02 into the pane,
 * where the program reads it, because the prefix is interpreted by a tmux client
 * and the client is the daemon. So the action is named instead of the keystroke.
 */
export interface TmuxActionMessage {
  type: "tmux";
  sessionId?: string;
  action: "split-right" | "split-down" | "new-window" | "rename";
  /** Only for `rename`. */
  name?: string;
}

/** Remote client → server: re-send the pane including paged scrollback. */
export interface ScrollbackRequest {
  type: "scrollback";
  sessionId: string;
  /**
   * Lines of history to request (server clamps to its own bound).
   * `0` (or omitted) means "re-send just the visible screen" — the return-to-
   * bottom action — and must not be read as "use the default".
   */
  lines?: number;
}

/**
 * A session as advertised to remote clients: the registry record minus the
 * daemon's internal bookkeeping, plus the freshness signal clients need.
 * Derived from `SessionRecord` so the two cannot drift apart.
 */
export type SessionInfo = Pick<
  SessionRecord,
  "id" | "kind" | "title" | "cwd" | "status" | "tmuxName" | "createdBy"
> & {
  /**
   * Last observed activity, epoch ms — the freshness signal for unread markers
   * (tmux `session_activity` for terminal sessions, record updates for RPC-only
   * ones). Absent when unknown.
   */
  lastActivityAt?: number;
};

/** Remote client → server: list sessions. */
export interface ListSessions { type: "list" }
/** Server → client: answer to `list`. */
export interface SessionsMessage { type: "sessions"; sessions: SessionInfo[] }

/**
 * Remote client → server: open a new session (FR-10 — from the phone, in any
 * directory). The server answers with a fresh `sessions` list.
 */
export interface NewSessionMessage {
  type: "new";
  kind?: "shell" | "pi";
  cwd?: string;
  title?: string;
}

/** Remote client → server: end a session. The server answers with a fresh list. */
export interface KillSessionMessage { type: "kill"; sessionId: string }

/**
 * The control-message union. Only these shapes are sent in v1: the v0.1 draft's
 * `HELLO`/`resumeToken`, `SESSION_STATE` and `CLOSE` were cut (docs/SPEC.md §6.2),
 * so they are not modelled here — a type nothing can send or receive is a
 * contract nobody can keep.
 */
/** Remote client → server: what is on the machine's desktop. */
export interface DesktopRequest { type: "desktop" }

/**
 * Remote client → server: put this shell on the desktop's screen.
 *
 * [workspace] is the desktop workspace to put it on, by the window manager's own
 * name. It is required in spirit: choosing where a window appears is the point,
 * and a default would mean a window could land on a desktop nobody is looking
 * at.
 */
export interface MakeLiveMessage { type: "makelive"; sessionId: string; workspace: string }

/** Remote client → server: take this shell's desktop window away. */
export interface CloseLiveMessage { type: "closelive"; sessionId: string }

/** Server → client: what came of a make-live or close-live request. */
export interface LiveResultMessage {
  type: "live";
  sessionId: string;
  ok: boolean;
  detail?: string;
}

/** Server → client: answer to `desktop`. */
export interface DesktopMessage { type: "desktop"; inventory: DesktopInventory }

export type ControlMessage =
  | Caps
  | Snapshot
  | Follow
  | Ping
  | Pong
  | ErrorMessage
  | Attach
  | Resize
  | MouseClick
  | Input
  | KeyMessage
  | CopyModeMessage
  | TmuxActionMessage
  | ScrollbackRequest
  | ListSessions
  | NewSessionMessage
  | KillSessionMessage
  | SessionsMessage
  | DesktopRequest
  | DesktopMessage
  | MakeLiveMessage
  | CloseLiveMessage
  | LiveResultMessage;