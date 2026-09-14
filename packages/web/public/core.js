/*
 * ldmux web client — pure core.
 *
 * Framework-free, DOM-free helpers shared by the client (app.js) and its tests.
 * It attaches one global: `LdmuxCore`. Keeping the protocol-critical logic here
 * means the frame decoder, the scroll state machine, and the snapshot repaint
 * math are unit-testable under `bun test` without a browser.
 */
(function (root) {
  "use strict";

  const HEADER_SIZE = 8;
  const OUTPUT_FRAME = 0x10;
  const MAX_FRAME_PAYLOAD = 16 * 1024 * 1024;

  /**
   * Decode as many complete frames as possible from `chunk` prefixed by `tail`.
   *
   * The wire format is the one canonical frame (docs/REMOTE.md): type u8,
   * flags u8, reserved u16, length u32 big-endian. A frame split across
   * WebSocket messages is reassembled via the returned `tail`. A length beyond
   * the 16 MiB ceiling poisons the stream (never grow the tail on bad input).
   *
   * @returns {{frames: Array<{type:number, flags:number, payload:Uint8Array}>,
   *            tail: Uint8Array, poisoned: boolean}}
   */
  function decodeFrames(tail, chunk, maxPayload = MAX_FRAME_PAYLOAD) {
    const bytes = chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk);
    const merged = new Uint8Array(tail.length + bytes.length);
    merged.set(tail);
    merged.set(bytes, tail.length);

    const frames = [];
    let offset = 0;
    let poisoned = false;
    while (merged.length - offset >= HEADER_SIZE) {
      const view = new DataView(merged.buffer, merged.byteOffset + offset, HEADER_SIZE);
      const type = view.getUint8(0);
      const flags = view.getUint8(1);
      const length = view.getUint32(4);
      if (length > maxPayload) {
        poisoned = true;
        break;
      }
      if (merged.length - offset - HEADER_SIZE < length) break;
      frames.push({
        type,
        flags,
        payload: merged.subarray(offset + HEADER_SIZE, offset + HEADER_SIZE + length),
      });
      offset += HEADER_SIZE + length;
    }
    return { frames, tail: poisoned ? new Uint8Array(0) : merged.subarray(offset), poisoned };
  }

  /** ADR-0004: at the bottom means following; anywhere above means anchored. */
  function scrollMode(viewportY, baseY) {
    return viewportY >= baseY ? "following" : "anchored";
  }

  /**
   * How far above the bottom the user is reading (0 when following). Used to
   * re-derive the anchor across a snapshot repaint, since xterm line ids are
   * not stable across buffer writes.
   */
  function anchorFromBottom(mode, viewportY, baseY) {
    return mode === "anchored" ? baseY - viewportY : 0;
  }

  /**
   * Where to scroll after a repaint. Returns -1 when the anchored line no
   * longer exists, which means "stand down to following" — never guess.
   */
  function anchoredTarget(baseY, fromBottom) {
    const target = baseY - fromBottom;
    return target < 0 ? -1 : target;
  }

  /** capture-pane cannot encode cursor position; restore it with this CSI. */
  function cursorSequence(cursorX, cursorY) {
    const row = Number(cursorY ?? 0) + 1;
    const col = Number(cursorX ?? 0) + 1;
    return `\x1b[${row};${col}H`;
  }

  function escapeHtml(value) {
    return String(value).replace(
      /[&<>"']/g,
      (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]),
    );
  }

  /** Extract the pairing token from the URL, the fragment, or storage. */  function tokenFrom(urlString, hash, stored) {
    let fromUrl = "";
    try {
      const url = new URL(urlString);
      fromUrl = url.searchParams.get("token") || "";
      if (!fromUrl && hash.startsWith("#token=")) fromUrl = hash.slice("#token=".length);
      return { token: fromUrl || stored || "", fromUrl, path: url.pathname };
    } catch {
      return { token: stored || "", fromUrl: "", path: "/" };
    }
  }

  /**
   * Unread marker: activity newer than the last time the user looked. Absent
   * timestamps never produce a marker (unknown is not "new").
   */
  function isUnread(lastActivityAt, seenAt) {
    const activity = Number(lastActivityAt ?? 0);
    const seen = Number(seenAt ?? 0);
    return activity > 0 && activity > seen;
  }

  /**
   * The DECSET/DECRST sequences that reproduce the pane's terminal modes in
   * xterm. `capture-pane` carries no mode information, so a snapshot without
   * this repaint leaves a TUI's mouse, application-cursor and bracketed-paste
   * state wrong (the fidelity gap ADR-0004 names).
   *
   * `allowMouse` is the user's choice: enabling mouse reporting in xterm makes
   * touch drags report to the program instead of scrolling the viewport, so the
   * phone keeps it off unless asked for (dooboxes MOBILE_TERMINAL_UX §5).
   */
  function modeSequences(state, allowMouse) {
    let out = "";
    out += state && state.bracketedPaste ? "\x1b[?2004h" : "\x1b[?2004l";
    out += state && state.appCursor ? "\x1b[?1h" : "\x1b[?1l";
    if (state && state.mouse && allowMouse) {
      // 1000 = button events, 1002 = button+motion, 1006 = SGR coordinates.
      out += "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
    } else {
      out += "\x1b[?1000l\x1b[?1002l\x1b[?1006l";
    }
    return out;
  }

  /**
   * Snapshots are delivered in order, but a paged-history capture can land
   * after a fresh attach snapshot; repainting from the older one would undo the
   * attach. Epoch 0/undefined means "no opinion" and is never stale.
   */
  function isStaleSnapshot(epoch, lastEpoch) {
    const now = Number(epoch ?? 0);
    const last = Number(lastEpoch ?? 0);
    return now > 0 && last > 0 && now < last;
  }

  root.LdmuxCore = {
    HEADER_SIZE,
    OUTPUT_FRAME,
    MAX_FRAME_PAYLOAD,
    decodeFrames,
    scrollMode,
    anchorFromBottom,
    anchoredTarget,
    cursorSequence,
    escapeHtml,
    tokenFrom,
    isUnread,
    modeSequences,
    isStaleSnapshot,
  };
})(typeof globalThis !== "undefined" ? globalThis : this);