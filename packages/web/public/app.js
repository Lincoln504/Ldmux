/*
 * ldmux web client — the reference TerminalSurface for phones.
 *
 * Renders the canonical tmux grid (ADR-0002) scaled to fit the viewport; never
 * resizes the server pane. Speaks the remote protocol in docs/REMOTE.md: JSON
 * control messages and binary OUTPUT frames (the one canonical frame format).
 */
"use strict";

const OUTPUT_FRAME = 0x10;
const HEADER_SIZE = 8;
const MAX_FRAME_PAYLOAD = 16 * 1024 * 1024;
const SCROLLBACK = 1000; // mobile memory bound (dooboxes MOBILE_TERMINAL_PERFORMANCE §15.1)
const PING_MS = 20000;

const $ = (id) => document.getElementById(id);
const els = {
  status: $("status"),
  title: $("title"),
  screen: $("screen"),
  wrap: $("term-wrap"),
  term: $("term"),
  empty: $("empty"),
  jump: $("jump"),
  drawer: $("drawer"),
  sessions: $("sessions"),
  keybar: $("keybar"),
  overlay: $("token-overlay"),
  tokenInput: $("token-input"),
};

let term = null;
let ws = null;
let sessions = [];
let currentId = null;
let pingTimer = null;
let reconnectDelay = 500;
let ctrlSticky = false;
// ADR-0004 scroll state: only explicit user intent changes the mode, and
// streaming output never moves the viewport.
let mode = "following";
let attachedOnce = false;

// ── token ────────────────────────────────────────────────────────────────
function readToken() {
  const url = new URL(location.href);
  const fromUrl = url.searchParams.get("token") || (url.hash.startsWith("#token=") ? url.hash.slice(7) : "");
  const stored = localStorage.getItem("ldmux.token") || "";
  const token = fromUrl || stored;
  if (fromUrl) {
    localStorage.setItem("ldmux.token", fromUrl);
    history.replaceState(null, "", url.pathname);
  }
  return token;
}
els.tokenInput.value = readToken();

// ── terminal ─────────────────────────────────────────────────────────────
function ensureTerm() {
  if (term) return term;
  term = new Terminal({
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, "DejaVu Sans Mono", monospace',
    fontSize: 14,
    scrollback: SCROLLBACK,
    cursorBlink: false,
    theme: { background: "#000000", foreground: "#c9d1d9" },
  });
  term.open(els.term);
  term.onData((data) => send({ type: "input", data }));
  // Scroll state machine: an upward move is user intent -> anchored.
  term.onScroll(() => updateScrollMode());
  return term;
}

function atBottom() {
  const buf = term && term.buffer && term.buffer.active;
  return !buf || buf.viewportY >= buf.baseY;
}

function updateScrollMode() {
  const next = atBottom() ? "following" : "anchored";
  if (next === mode) {
    els.jump.classList.toggle("hidden", mode === "following");
    return;
  }
  mode = next;
  els.jump.classList.toggle("hidden", mode === "following");
  send({ type: "follow", mode });
}

function jumpToBottom() {
  mode = "following";
  els.jump.classList.add("hidden");
  if (term) term.scrollToBottom();
  send({ type: "follow", mode });
}

/** Scale the canonical grid to fit the viewport (never reflow the server). */
function scaleToFit() {
  if (!term || !term.element) return;
  els.wrap.style.transform = "scale(1)";
  const natural = term.element.querySelector(".xterm-screen");
  if (!natural) return;
  const nw = natural.offsetWidth;
  const nh = natural.offsetHeight;
  const cw = els.screen.clientWidth;
  const ch = els.screen.clientHeight;
  if (!nw || !nh || !cw || !ch) return;
  const scale = Math.min(cw / nw, ch / nh);
  els.wrap.style.transform = `scale(${scale})`;
}
addEventListener("resize", scaleToFit);
addEventListener("orientationchange", () => setTimeout(scaleToFit, 150));

// ── framing ──────────────────────────────────────────────────────────────
let tail = new Uint8Array(0);
function onBinary(buf) {
  const bytes = new Uint8Array(buf);
  const merged = new Uint8Array(tail.length + bytes.length);
  merged.set(tail);
  merged.set(bytes, tail.length);
  let offset = 0;
  let poisoned = false;
  while (merged.length - offset >= HEADER_SIZE) {
    const view = new DataView(merged.buffer, merged.byteOffset + offset, HEADER_SIZE);
    const type = view.getUint8(0);
    const length = view.getUint32(4);
    // Same DoS guard as the wire codec: never let a bogus length grow the tail.
    if (length > MAX_FRAME_PAYLOAD) {
      poisoned = true;
      break;
    }
    if (merged.length - offset - HEADER_SIZE < length) break;
    const payload = merged.subarray(offset + HEADER_SIZE, offset + HEADER_SIZE + length);
    if (type === OUTPUT_FRAME && term) term.write(new TextDecoder().decode(payload));
    offset += HEADER_SIZE + length;
  }
  if (poisoned) {
    setStatus("protocol error", "err");
    tail = new Uint8Array(0);
    send({ type: "list" });
    return;
  }
  tail = merged.subarray(offset);
}

// ── protocol ─────────────────────────────────────────────────────────────
function send(msg) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function setStatus(text, cls) {
  els.status.textContent = text;
  els.status.className = cls || "";
}

function connect() {
  const token = readToken();
  if (!token) {
    els.overlay.classList.remove("hidden");
    return;
  }
  els.overlay.classList.add("hidden");
  const proto = location.protocol === "https:" ? "wss" : "ws";
  ws = new WebSocket(`${proto}://${location.host}/ws?token=${encodeURIComponent(token)}`);
  ws.binaryType = "arraybuffer";

  ws.addEventListener("open", () => {
    reconnectDelay = 500;
    setStatus("online", "");
    send({ type: "list" });
    if (currentId) send({ type: "attach", sessionId: currentId });
    clearInterval(pingTimer);
    pingTimer = setInterval(() => send({ type: "ping", id: Date.now() }), PING_MS);
  });

  ws.addEventListener("message", (ev) => {
    if (typeof ev.data === "string") {
      try {
        onControl(JSON.parse(ev.data));
      } catch {
        /* ignore malformed control messages */
      }
      return;
    }
    onBinary(ev.data);
  });

  ws.addEventListener("close", () => {
    clearInterval(pingTimer);
    setStatus("reconnecting…", "err");
    setTimeout(connect, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, 8000);
  });
  ws.addEventListener("error", () => setStatus("error", "err"));
}

function onControl(msg) {
  switch (msg.type) {
    case "sessions":
      sessions = msg.sessions || [];
      renderSessions();
      break;
    case "caps":
      ensureTerm();
      term.resize(msg.cols, msg.rows);
      setTimeout(scaleToFit, 0);
      els.empty.style.display = "none";
      break;
    case "snapshot":
      applySnapshot(msg);
      break;
    case "error":
      if (msg.code === "resync" || msg.code === "too_far_behind") {
        if (currentId) send({ type: "attach", sessionId: currentId });
      } else if (msg.code === "unknown_session" || msg.code === "session_dead" || msg.code === "attach_failed") {
        currentId = null;
        els.title.textContent = "ldmux";
        els.empty.style.display = "";
        setStatus(msg.code, "err");
        renderSessions();
      } else {
        setStatus(msg.code, "err");
      }
      break;
    default:
      break;
  }
}

// ── snapshot application (ADR-0004) ──────────────────────────────────────
/**
 * Repaint from a server snapshot without destroying the user's scrollback or
 * yanking an anchored viewport. `term.reset()` is reserved for the very first
 * attach of a session.
 */
function applySnapshot(msg) {
  ensureTerm();
  const buf = term.buffer && term.buffer.active;
  const anchorFromBottom = mode === "anchored" && buf ? buf.baseY - buf.viewportY : 0;

  if (!attachedOnce) {
    attachedOnce = true;
    term.reset();
  } else {
    // Clear the visible screen and home the cursor: the previous screen scrolls
    // into history instead of being erased.
    term.write("\x1b[2J\x1b[H");
  }
  term.write(msg.screen || "");
  // capture-pane cannot encode cursor position; restore it from the snapshot.
  const row = Number(msg.cursorY ?? 0) + 1;
  const col = Number(msg.cursorX ?? 0) + 1;
  term.write(`\x1b[${row};${col}H`);

  if (mode === "anchored") {
    const b = term.buffer && term.buffer.active;
    if (b) {
      const target = b.baseY - anchorFromBottom;
      if (target < 0) {
        // The anchored line is gone: stand down to following (never guess).
        jumpToBottom();
      } else {
        term.scrollToLine(target);
      }
    }
  }
}

// ── sessions drawer ──────────────────────────────────────────────────────
function renderSessions() {
  els.sessions.innerHTML = "";
  for (const s of sessions) {
    const li = document.createElement("li");
    li.className = s.id === currentId ? "active" : "";
    const attachable = s.status === "running" && s.tmuxName;
    li.innerHTML =
      `<div class="s-title"><span class="s-kind">${s.kind}</span>${escapeHtml(s.title || s.id)}</div>` +
      `<div class="s-meta">${escapeHtml(s.cwd || "")} · ${s.status}${attachable ? "" : " · rpc"}</div>`;
    li.addEventListener("click", () => {
      if (attachable) attach(s.id);
      els.drawer.classList.add("hidden");
    });
    els.sessions.appendChild(li);
  }
  if (!sessions.length) els.sessions.innerHTML = '<li class="s-meta">no sessions</li>';
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function attach(sessionId) {
  currentId = sessionId;
  attachedOnce = false;
  mode = "following";
  els.jump.classList.add("hidden");
  const s = sessions.find((x) => x.id === sessionId);
  els.title.textContent = s ? `${s.kind} · ${s.title}` : "ldmux";
  send({ type: "attach", sessionId });
  renderSessions();
}

function cycle(delta) {
  const usable = sessions.filter((s) => s.status === "running" && s.tmuxName);
  if (usable.length < 2) return;
  const i = usable.findIndex((s) => s.id === currentId);
  const next = usable[(i + delta + usable.length) % usable.length];
  attach(next.id);
}

// ── extra keys ───────────────────────────────────────────────────────────
const KEYS = [
  ["Esc", () => send({ type: "key", keys: ["Escape"] })],
  ["Tab", () => send({ type: "key", keys: ["Tab"] })],
  ["Ctrl", () => toggleCtrl()],
  ["↑", () => send({ type: "key", keys: ["Up"] })],
  ["↓", () => send({ type: "key", keys: ["Down"] })],
  ["←", () => send({ type: "key", keys: ["Left"] })],
  ["→", () => send({ type: "key", keys: ["Right"] })],
  ["C-c", () => send({ type: "key", keys: ["C-c"] })],
  ["C-d", () => send({ type: "key", keys: ["C-d"] })],
  ["|", () => send({ type: "input", data: "|" })],
  ["-", () => send({ type: "input", data: "-" })],
  ["/", () => send({ type: "input", data: "/" })],
];
function buildKeybar() {
  for (const [label, fn] of KEYS) {
    const b = document.createElement("button");
    b.textContent = label;
    b.addEventListener("click", (e) => {
      e.preventDefault();
      fn();
    });
    els.keybar.appendChild(b);
  }
}
function toggleCtrl() {
  ctrlSticky = !ctrlSticky;
  els.keybar.querySelectorAll("button").forEach((b) => {
    if (b.textContent === "Ctrl") b.classList.toggle("active", ctrlSticky);
  });
}
// Sticky Ctrl: translate the next typed character into C-<char>.
addEventListener("keydown", (e) => {
  if (!ctrlSticky || !e.key || e.key.length !== 1) return;
  e.preventDefault();
  e.stopPropagation();
  ctrlSticky = false;
  toggleCtrl();
  const code = e.key.toUpperCase();
  if (code >= "A" && code <= "Z") send({ type: "key", keys: [`C-${code.toLowerCase()}`] });
}, true);

// ── swipe between sessions (edge strips only) ────────────────────────────
function edgeSwipe(el) {
  let start = null;
  el.addEventListener("touchstart", (e) => {
    if (e.touches.length === 1) start = { x: e.touches[0].clientX, y: e.touches[0].clientY };
  }, { passive: true });
  el.addEventListener("touchend", (e) => {
    if (!start) return;
    const dx = e.changedTouches[0].clientX - start.x;
    const dy = e.changedTouches[0].clientY - start.y;
    start = null;
    if (Math.abs(dx) > 60 && Math.abs(dx) > Math.abs(dy) * 1.5) cycle(dx < 0 ? 1 : -1);
  }, { passive: true });
}
edgeSwipe($("swipe-left"));
edgeSwipe($("swipe-right"));

els.jump.addEventListener("click", jumpToBottom);

// ── wiring ───────────────────────────────────────────────────────────────
$("menu").addEventListener("click", () => {
  send({ type: "list" });
  els.drawer.classList.toggle("hidden");
});
$("refresh").addEventListener("click", () => send({ type: "list" }));
$("keys-toggle").addEventListener("click", () => els.keybar.classList.toggle("hidden"));
$("token-form").addEventListener("submit", (e) => {
  e.preventDefault();
  localStorage.setItem("ldmux.token", els.tokenInput.value.trim());
  els.overlay.classList.add("hidden");
  connect();
});

buildKeybar();
ensureTerm();
els.empty.style.display = "";
connect();

// Installable app shell (offline cache; /ws is never cached by the worker).
if ("serviceWorker" in navigator && location.protocol === "https:") {
  addEventListener("load", () => {
    navigator.serviceWorker.register("/sw.js").catch(() => {
      /* offline shell is a bonus, never a requirement */
    });
  });
}