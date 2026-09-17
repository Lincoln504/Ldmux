/**
 * JSON control messages carried as text frames (see frames.ts). Pane output
 * travels as binary OUTPUT frames; everything in the other direction is one of
 * these JSON shapes.
 */

import type { DesktopInventory, DeviceInfo, SessionOwner, SessionRecord } from "./rpc.ts";

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

/** Remote client → server: who this connection is (ADR-0010).
 *
 * Sent once, before anything else. Ownership is keyed on the connection plus
 * this identity, and a device's claims are dropped when the connection ends —
 * which is the whole defence against attachment state outliving the socket.
 */
export interface Hello { type: "hello"; device: DeviceInfo }

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
  /**
   * The user was told who holds this shell and chose to take it anyway.
   *
   * Attachment is never a silent steal (ADR-0010): a plain attach that finds
   * another owner is *refused*, and the client is answered with who holds it so
   * it can ask. Only a second attach carrying this flag displaces them.
   */
  takeover?: boolean;
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
  /**
   * What the pane is currently running (`bash`, `vim`, `pi`), when known.
   *
   * The launcher shows this so a row says what a shell *is*, not only where it
   * is: two entries in the same directory are otherwise indistinguishable, and
   * "which of these is my editor" is the question the list exists to answer.
   */
  process?: string;
  /**
   * The device holding this shell, when one does (ADR-0010).
   *
   * Sent so the launcher can show *where* a shell is open from a fact, rather
   * than from a window-manager probe it would otherwise have to guess with.
   */
  owner?: SessionOwner;
  /**
   * The desktop workspace this shell has a window on, when it has one.
   *
   * This is what makes "closing on the phone leaves it open on the desktop"
   * decidable: it is set when `makeLive` succeeds and cleared when the window is
   * closed or the session ends. The phone previously inferred it from a WM probe
   * cached for one second, which is wrong in both directions — a window that had
   * just closed still looked open, and one that had just opened still looked
   * absent.
   */
  liveWorkspace?: string;
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
  /**
   * What to run in it, as argv.
   *
   * The shell is the *host's* login shell either way (docs/SPEC.md §7) — this is
   * the program started inside it, so a new shell can be a shell or an editor or
   * an agent without the client having to know how to spell any of them.
   */
  command?: string[];
}

/** Remote client → server: end a session. The server answers with a fresh list. */
export interface KillSessionMessage { type: "kill"; sessionId: string }

/**
 * Remote client → server: the user closed this shell in the UI.
 *
 * Deliberately **not** `kill` (ADR-0010). Closing lets go of a shell, and
 * whether that also *ends* it depends on whether something else is holding it
 * open — a fact only the server has, so the client cannot be the authority on
 * it. `kill` stays the unconditional end: the CLI uses it, and so does a desktop
 * window being closed.
 */
export interface CloseSessionMessage { type: "close"; sessionId: string }

/**
 * Server → client: a shell is held by another device, or was taken from you.
 *
 * Sent both when an attach is refused and when an attach that already succeeded
 * is displaced — the same situation from the user's point of view, so the same
 * message. `owner` is null when nobody holds it (the holder left).
 */
export interface OwnedMessage {
  type: "owned";
  sessionId: string;
  owner: SessionOwner | null;
}

/**
 * The answer to a click: the server put it into a pane whose program asked for
 * mouse input, so the click is the tap and nothing else should follow it.
 *
 * Without this the client could only tell a *refusal* from nothing at all, and a
 * click that worked looked identical to a click that vanished — so the client's
 * refusal fallback, which exists for the second case, also ran for the first.
 * Measured consequence: one tap moved the cursor twice. `micro` clamped the click
 * to the end of the line it was on (`col 41 -> col 7`), then 1.2 s later the
 * fallback walked back toward the tapped cell from a stale frame cursor, landed
 * on an empty line and clamped to the gutter — `(2,4)` for a tap that should have
 * ended at `(7,2)`.
 *
 * Deliberately not called "success": this is not "the program moved its cursor",
 * which the server cannot know. It is "a program that asked for mouse input was
 * given this click", which is the fact the client needs to decide whether to do
 * anything else.
 */
export interface MouseDeliveredMessage {
  type: "mousedelivered";
  col: number;
  row: number;
}

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
  | Hello
  | Follow
  | Ping
  | Pong
  | ErrorMessage
  | Attach
  | OwnedMessage
  | MouseDeliveredMessage
  | Resize
  | MouseClick
  | Input
  | KeyMessage
  | CopyModeMessage
  | ScrollbackRequest
  | ListSessions
  | NewSessionMessage
  | KillSessionMessage
  | CloseSessionMessage
  | SessionsMessage
  | DesktopRequest
  | DesktopMessage
  | MakeLiveMessage
  | CloseLiveMessage
  | LiveResultMessage;