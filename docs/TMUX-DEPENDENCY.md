# The tmux dependency

**Status: open decision.** The user's steer is to keep tmux as the PTY keeper and
investigate replacing the control-mode dependency with something of our own. The second
option — building the session layer ourselves — was asked to be investigated in depth
and is written up here as far as the evidence goes. **Nothing has been decided and
nothing has been started.**

---

## 1. The question

ldmux drives **its own tmux server** (`-L ldmux`, never the user's `default`) through
tmux's **control mode** (`tmux -C attach`), plus a set of ordinary tmux commands. tmux
holds the PTYs and therefore the shells.

That is three separate dependencies bundled into one sentence, and they can be taken
apart:

1. **tmux as the process that owns the PTYs** — sessions and shells outlive our daemon.
2. **tmux as the source of terminal state** — its screen is what `capture-pane` reads and
   what our snapshots are built from.
3. **tmux's control-mode protocol as our transport** — `%output` carries raw bytes to us.

The question is which of those to keep, and in particular whether (3) should be replaced
by ordinary commands, and whether (1) should become ours.

---

## 2. What we depend on, exactly

Enumerated from the code, not from memory. `packages/daemon/src/tmux/control-plane.ts`
and `control-parser.ts`.

### 2.1 Control mode (protocol)

| Feature | Used for | Ordinary-command equivalent |
|---|---|---|
| `%output` | raw pane bytes, the whole live stream | `pipe-pane` — **unverified** (§6.1) |
| `%begin`/`%end`/`%error` | command/response framing over the same pipe | exit code of a normal invocation |
| `%pause`/`%continue` | flow control when a viewer cannot keep up | none known |
| `%extended-output` | output with an age stamp, for the same purpose | none known |
| `%exit` | the client died | watching the process |
| `refresh-client -f pause-after=N` | asking for flow control at all | none known |

The parser is a hand-written state machine over a **text** protocol with octal-escaped
binary payloads (`octal.ts`). It is not versioned by tmux and has no specification
beyond a community-maintained wiki page.

### 2.2 Ordinary commands

`set-option`, `set-environment`, `send-keys`, `attach`, `refresh-client`, `list-panes`,
`list-sessions`, `list-clients`, `new-session`, `kill-session`, `has-session`,
`capture-pane`, `display-message`, `resize-window`, `start-server`.

These are stable, documented, and have been for years. **This is the half that is not
the problem.**

---

## 3. What tmux does for us

The honest ledger, because this is what the alternative has to rebuild.

| Capability | Rebuild cost |
|---|---|
| PTY allocation, lifecycle, reaping | Medium. `forkpty`/`openpty` is a solved problem; getting the supervision right is not. |
| **Sessions outlive the daemon** | **High.** This is the whole reason tmux is here. Confirmed working: `KillMode=process` plus tmux means a daemon restart does not touch a shell. |
| Resize negotiation between clients of different sizes | Medium. We use `window-size manual` and drive it ourselves, deliberately. |
| Screen text extraction (`capture-pane`) | Low with an emulator in hand; we already ship one. |
| Key injection (`send-keys`) | Low. Note `send-key` semantics differ from writing bytes. |
| Mouse synthesis | Low; we already synthesise SGR clicks ourselves. |
| Scrollback storage and paging | Medium. Memory is the real cost, and tmux's `history-limit` is doing it today. |
| Flow control (`pause-after`, `%pause`/`%continue`) | **High.** This is the part of control mode with no ordinary-command answer. |
| Interpreting the outer terminal (TERM/terminfo) | Done for us, and it is also where two of the annoyances come from (§4.2). |

Embedding a **VT engine does not remove this ledger** — it removes the hardest *single*
component (escape-sequence parsing and terminal state) and leaves the supervision. We
already have that component: libghostty-vt is in the client today.

---

## 4. What depending on it has actually cost

Measured on this machine during the 2026-09-19 session. These are the concrete data
points the decision should weigh, and each is a real defect or a real surprise.

### 4.1 The lifecycle is not ours, and the failure is silent

- **`set-option` does not start a tmux server.** Against a missing one:
  `error connecting to … (No such file or directory)`, exit 1.
- **A tmux server with no sessions exits.** Options set on an empty server are therefore
  gone before the first session exists, and that session is created by a *new* server on
  the defaults.
- **tmux reads its configuration only when a server starts.** A long-lived server keeps
  the profile it was born with.

Together: our twenty-option profile was **skipped in silence on every cold start**, and
the only symptom on the phone was pi saying "tmux extended keys is off" — with no
`history-limit`, `mouse`, `set-clipboard`, `escape-time`, `focus-events` or `COLORTERM`
in force either. Fixed by serialising the profile to a file and passing `-f` on every
spawn (`docs/STATE.md` §4.3).

**This is the shape of the cost.** Not "tmux is buggy" — it is that the lifetime that
matters (the server's) belongs to another process, so our configuration is applied
through a window we do not control, and when it misses, nothing says so.

### 4.2 Options are a compatibility surface

- `extended-keys` and `allow-passthrough` exist in 3.5a and 3.7b and would fail silently
  on anything older.
- `window-size manual` is load-bearing (it stops tmux filling a wider window with
  `fill-character`), and its absence changes what a test sees.
- `TERM` and `TMUX` leak into every child unless actively stripped; a daemon started from
  inside the user's own tmux passes *their* `TMUX` down, which makes a program inside an
  ldmux shell believe it is in the user's session.
- The `default-shell`/`PS1`/`BASH_ENV` family had to be neutralised one by one to get a
  prompt that comes from the user's own startup files.
- `escape-time` had to be set to 0 because on a phone it was 500 ms of nothing.

Each of these was discovered by a symptom, not by reading a specification.

### 4.3 The default configuration is not the one we want

The phone complained first, and it was right: an unconfigured tmux is a worse terminal
than this app is capable of being. `docs/SPEC.md` §5.2 now carries a twenty-line profile
that exists only to make tmux behave the way a terminal should. Every line of it is a
thing we would not have to write if the layer were ours.

### 4.4 What is *not* a problem

Worth stating, because it is the other half of the argument.

- The ordinary commands are stable, well documented, and have not changed under us.
- tmux has been correct about pty lifetimes, orphaned processes and signal handling for
  twenty years, and we have not had to think about any of it.
- Sessions surviving our daemon's restart is *measured*, repeatedly, and it is the single
  most valuable behaviour in the system.
- Underlying it all: the user's sessions are real work. A layer we wrote would have to
  earn the trust tmux already has.

---

## 5. The options

### (a) Keep tmux as the holder, replace control mode with ordinary commands

- Sessions and PTYs stay tmux's. The transport becomes `pipe-pane` (streaming) plus the
  ordinary commands we already use (everything else).
- **Gains**: removes the version-sensitive, unspecified, octal-escaped text protocol and
  its hand-written state machine. Keeps the behaviour that matters most.
- **Costs**: flow control has no known ordinary-command equivalent, so `pause-after`,
  `%pause`/`%continue` and the backpressure story would have to be rebuilt on top of
  whatever `pipe-pane` does when its reader is slow — **which is not known** (§6.1). The
  polling fallback already exists and is a known-good degradation.
- **Risk**: medium. The failure mode of a slow `pipe-pane` reader could be dropped output.

### (b) Keep everything as it is

- **Gains**: nothing to build; it works.
- **Costs**: the flaws in §4 remain, including the one that is currently *fixed by a
  file* rather than designed away, and the protocol risk stays.
- **Risk**: low, bounded by tmux's own stability.

### (c) Build the session layer ourselves

- A daemon that owns the PTYs, with a separate **keeper** process so a restart does not
  kill shells (the pattern VS Code uses with its pty host), a libghostty-vt instance per
  session for state and scrollback, and our own framed protocol.
- **Gains**: one fewer external dependency and one fewer language; a first-class protocol
  instead of an unspecified one; scrollback and persistence semantics that are ours to
  define; no option profile at all, because there are no options; and a security posture
  we set rather than inherit.
- **Costs**: everything in §3, plus the keeper design, plus the fact that the *client*
  and the *daemon* would then both need the native engine (the client already does; the
  daemon would gain a native component).
- **Risk**: high. This is the option that can lose a user's work if it is wrong.

---

## 6. What is not known

The web-research passes for this decision failed mid-session (search timeout, then a
cancelled run), so the following are **not** answered here. Each has the question stated
precisely enough to be answered cheaply, and none of them needs to be answered before
option (b) or (a) can proceed.

### 6.1 The `pipe-pane` questions — the ones that decide option (a)

1. Does `pipe-pane -o 'cat >> file'` deliver the pane's bytes **verbatim**, including
   escape sequences, OSC 52, OSC 8 and multi-byte characters? *Method:* scratch socket,
   `printf` a known sequence into a pane, diff the captured bytes.
2. What does tmux do when the reader is slow — block the pane, buffer, or drop?
   *Method:* a reader that sleeps, a producer that floods, count the bytes.
3. Does `pipe-pane` output carry ordering or sequence numbers relative to
   `capture-pane` snapshots? *Method:* interleave a capture and a stream and look.
4. Is there any ordinary-command substitute for `pause-after`? *Method:* read the tmux
   manual's `refresh-client` and `pipe-pane` sections rather than guessing.

### 6.2 The keeper questions — the ones that decide option (c)

5. How do VS Code's pty host, WezTerm's mux server, Zellij's server, `abduco` and `dtach`
   keep PTYs alive across a restart of the client-facing process, and is output buffered
   while nothing is attached?
6. Is SCM_RIGHTS descriptor passing used anywhere in that pattern, and does it help?
7. `node-pty` versus a Rust pty crate (`portable-pty`, `nix::pty`): maintenance, failure
   modes, and the reason for `openpty failed` on some hosts.
8. napi-rs: prebuilt binaries, N-API ABI stability across Node versions, async and
   threads, packaging. This matters only if option (c) is chosen and the engine lives in
   the daemon.

### 6.3 Product questions

9. What does the phone's own flow control need? We attach with a `pause-after=2` today;
   whether a phone on a bad link needs something better is unmeasured.

---

## 7. Recommendation

**Option (a): keep tmux as the PTY keeper, and stop depending on control mode — but only
after §6.1 is answered.** In order:

1. **Answer the four `pipe-pane` questions by measurement.** They are cheap, they are
   local, and they decide whether (a) is a good idea or a trap.
2. **If `pipe-pane` carries bytes verbatim and degrades safely**, build the streaming path
   on it and keep control mode as the fallback (the polling fallback already exists, so a
   third mode is not a new concept in this codebase).
3. **Do not build (c) until the keeper design is written down and reviewed on its own.**
   The plan comes first — a separate process that owns the PTYs, what happens to buffered
   output while nobody is attached, how a shell's scrollback survives, and how a restart
   is made safe — and it gets its own ADR before any code.
4. **Keep the profile file regardless of the outcome.** It is correct under every option
   that involves tmux, and it is the fix for a defect that was losing the entire
   configuration.

**Why not (c) yet**: §3 is a real ledger, and the one line that matters most — sessions
outliving the daemon — is currently free and measured. The VT engine being available
makes (c) *feasible*; it does not make it *cheap*, because the engine was never the hard
part of this layer. The hard part is supervision, and a mistake there costs the user
their work rather than a test failure.

**Why not (b)**: the control-mode protocol is the part of tmux with no specification and
no version contract, it is the part we parse by hand, and it is the part whose failure
modes are least likely to announce themselves. It is also, per §6.1, potentially the
easiest to replace.

### The question for the user

> Do you want (a) — tmux keeps the PTYs, we replace control mode with `pipe-pane` and
> ordinary commands, starting by measuring what `pipe-pane` actually does — or should I
> write the keeper-process plan for (c) so you can judge it before either is built?
> Either way nothing gets started until you say so.
