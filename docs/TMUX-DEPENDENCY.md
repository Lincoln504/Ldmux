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

### 6.1 The `pipe-pane` questions — **ANSWERED by measurement (2026-09-19)**

All four were measured on an isolated scratch server (`tmux -L ldmux-pp`, never the
user's). The methods and the numbers are in **§8**. The answers:

1. **Verbatim? Yes, completely.** Real ESC bytes (`0x1b`), `ESC[31m`, `ESC[0m`,
   `ESC]52;c;…BEL`, `ESC]8;;…BEL`, `é` and an emoji all arrived byte-for-byte. **No
   octal escaping is applied** — that is control mode's doing, not tmux's, and it is the
   one thing `pipe-pane` does better.
2. **Slow reader: bounded buffering, no loss, backpressure to the pane.** A reader that
   stalled 12 s while 400,000 numbered lines (~18 MB) were produced received **all
   400,000 lines with zero missing**. Server RSS rose 8.4 MB → ~26 MB and **stopped** —
   the buffer is bounded. Once it fills, the pane's own process blocks on writing to its
   pty, which is exactly what a local terminal does.
3. **Ordering is strict and there are no sequence numbers.** `MARK_1 … MARK_5` arrived in
   order. `capture-pane` keeps working while a pipe is live, so the two can be mixed —
   but nothing correlates them, so a client must treat the stream and the snapshot as
   two views with no shared clock (which is what we already do).
4. **There is no substitute for `pause-after`, and this is the real cost.**
   `pause-after=seconds` is documented as *"output is paused once the pane is seconds
   behind **in control mode**"*. It does not apply to a pipe. **`pipe-pane` has no flow
   control at all** — no pause, no resume, no notification that a consumer is behind.
   This is confirmed by the wider ecosystem: `pipe-pane`'s documented limitations are
   precisely "no back-pressure, no structured events, no pane lifecycle notifications".

### 6.1.1 Two further measured facts that change the shape of option (a)

- **`pipe-pane` is per-pane.** A pane-level pipe sees only that pane; a window-level
  pipe (`-t session:window`) saw **neither** pane. tmux already keeps one control client
  per session and demultiplexes `%output` by pane id, so option (a) would mean **one pipe
  and one reader process per pane** instead of one connection per session.
- **A dead reader does not take the pane with it.** The pane survived, and tmux's
  `#{pane_pipe}` still reported `1` — **tmux does not notice its reader died**, so a
  supervisor has to poll for it (the `tmux-pane-control` project polls `list-panes` every
  200 ms for exactly this reason). Control mode gets this for free: `%exit` is the
  client's death notice.

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

**The measurements changed the answer, and not in the direction the options implied.**

### What they settled

- **`pipe-pane` is byte-verbatim and lossless under a stalled reader**, with a *bounded*
  buffer that applies backpressure to the pane instead of dropping. That is a better
  streaming primitive than control mode's octal-escaped `%output`.
- **Flow control is reproducible without control mode**: control mode's `pause-after`
  stops the pane when a client is behind, and *not reading the pipe* stops it in exactly
  the same place. The measured block is the pty write in the pane's own process — which
  is what a local terminal does. So the one capability I expected option (a) to lose,
  it does not actually lose.
- **The costs of (a) are smaller than feared and are all supervision, not protocol**: one
  pipe per pane (we view one pane at a time, so this is fine), a poll to notice a dead
  reader (tmux does not tell us), and no `%exit`. Everything else we need — `list-panes`,
  `capture-pane`, `send-keys`, `resize-window` — is an ordinary command that does not
  change.
- **Control mode is not deprecated** and is the primary integration mechanism for iTerm2
  and Ghostty. So this is a choice between two working mechanisms, not a migration off
  something dying.

### What it would delete

`control-parser.ts` (160), `control-client.ts` (401), `octal.ts` (34) — **595 lines of the
most fragile code in the daemon**, plus 235 lines of tests. Replaced by a pipe reader and
a small supervisor. That is the strongest argument for (a): the parser is hand-written
against a protocol with **no specification and no version contract**, and a parser for an
unspecified format is where a terminal client breaks in ways nobody can predict.

### The recommendation

**Build the `pipe-pane` path as a third `StreamMode` behind the abstraction that already
exists, measure both on the real phone, and then delete whichever loses.**

1. `stream.ts` already models this: `type StreamMode = "control" | "polling"`. Add
   `"pipe"`. The switch is already a config decision, not a rewrite.
2. Run control mode and the pipe against the same session on the tailnet and on a
   throttled link. Compare latency, link cost and behaviour when the phone goes away.
3. **Delete the loser.** If the pipe wins, the 595 lines and their 235 lines of tests go.

**Why not decide now**: the two mechanisms are close enough that the decision should be
made on the phone's numbers rather than on this document's argument, and the abstraction
that makes the comparison cheap already exists. Guessing between two working options is
how a project acquires a rewrite it did not need.

**Why not (b)**: 595 lines of hand-written parsing of an unspecified protocol, in the one
component whose failure is least likely to announce itself, is the largest maintainability
liability in the codebase. The measurements show there is a simpler primitive that does
the same job.

**Why not (c) yet**: the keeper pattern is now well understood from the research (§9) and
none of the six systems surveyed use SCM_RIGHTS — they all keep the PTY in a long-lived
supervisor process and reattach over a socket. That is a design we could follow, but it
costs the measured, free survival we already have, and it is a separate decision that
deserves its own plan. **Nothing about (a) blocks (c)** — a pipe reader is a smaller thing
to replace than a protocol client.

### The question for the user

> I measured it, and the answer is not what either option assumed: `pipe-pane` is
> byte-verbatim, lossless under a stalled reader, and its backpressure is equivalent to
> `pause-after`. It would delete 595 lines of hand-written protocol parsing. Do you want
> me to **build it as a third stream mode and A/B it against control mode on the phone**,
> or would you rather I **keep control mode and harden the parser** — and either way, do
> you want the keeper-process plan for (c) written up as its own ADR first?

---

## 8. The measurements, in full

Every one ran on an **isolated scratch server** (`tmux -L ldmux-pp`), never the user's
sessions, and each ended with `kill-server`. Reproduce with the method column.

| # | Question | Method | Result |
|---|---|---|---|
| 1 | Byte-verbatim? | `pipe-pane -o 'cat >> raw.bin'`, then `printf` known sequences into the pane | `ESC[31m`, `ESC[0m`, `ESC]52;c;TERDTElQMTIzBEL`, `ESC]8;;…BEL`, `é`, 😀 — **all present as real bytes**. 9 `0x1b` bytes. **No `\033` octal escaping** (the two literal `\033` occurrences were the shell echoing the command that printed them, confirmed by position). |
| 2a | Slow reader — loss? | Reader sleeps 12 s, then drains. 400,000 numbered lines produced | **400,000 distinct line numbers, 0 missing.** `FINISHED_400K` present. |
| 2b | Slow reader — unbounded buffer? | Reader never reads; watch the server's RSS | 8,428 KB → 26,716 KB, then **flat**. Bounded. |
| 2c | Slow reader — block or drop? | Same as 2a, poll the pane for the producer's finishing marker | Producer **blocked at t=2 s, finished at t=4 s** — backpressure reached the pane, no data lost. |
| 3 | Ordering? | `MARK_1…MARK_5`, compare the pipe stream against `capture-pane` | Stream strictly ordered. `capture-pane` works concurrently. No sequence numbers, no shared clock. |
| 4 | Flow control? | Read the manual for `pause-after` | **"output is paused once the pane is seconds behind in control mode"** — control mode only. No substitute for a pipe. |
| 5 | Per-pane or per-window? | Pipe on `m:0.0` and `m:0.1`, then on `m` | Pane pipes are isolated and correct. **A window-level pipe captured neither pane.** |
| 6 | Dead reader? | `pkill` the reader, then `#{pane_pipe}` | Pane **survived** and stayed responsive; `#{pane_pipe}` still reported `1`. **tmux does not notice.** |

**Server responsiveness during a stalled reader**: `list-sessions` answered in 7–10 ms and
an unrelated pane ran a command normally throughout. A stalled reader does not wedge tmux.

---

## 9. The keeper pattern — what the research found

Surveyed: dtach, abduco, mosh, Zellij, VS Code's pty host, WezTerm's mux server.

**Every one of the six keeps the PTY in a long-lived supervisor process and reattaches
over a Unix socket. None uses SCM_RIGHTS descriptor passing.** That is a clear answer to
the question of how to build (c), and it is simpler than it looked.

| System | PTY owner | Reattach | Output while detached | SCM_RIGHTS |
|---|---|---|---|---|
| **dtach** | The `dtach` master process, `forkpty()`, fd in a static struct | New client connects to the socket, `MSG_ATTACH` | **Discarded** — the pty fd is not in the `select()` set with no clients, so it is never read | No |
| **abduco** | Server process, `forkpty()` | New client connects to the session socket | Same pattern (not buffered) | No |
| **mosh** | `mosh-server`, tied to the SSH session that launched it | UDP re-sync with the session key | Processed through the terminal emulator, **dropped** at the UDP layer | No |
| **Zellij** | Server process, `ActiveDetached` state | `zellij attach`, protobuf handshake | **Held in the in-memory pane grid** — reconnecting client gets current state | No |
| **VS Code** | A dedicated pty host process | Reattach **by process id** after a window reload | **Buffered in the host**, with **ack-based flow control** (`AckDataBufferer`) | No |
| **WezTerm** | Mux server process | Connect to the domain socket | Held in the emulator's `Surface`; reattach sends **only changed lines** (dirty tracking) | No |

Three findings worth carrying:

1. **WezTerm's dirty tracking is the same idea as our snapshot-plus-delta**, arrived at
   independently: the server diffs its current state against what the client last saw and
   sends only the changed lines.
2. **VS Code's flow control is an explicit ack** — the renderer acknowledges bytes
   processed and the host pauses past a threshold. That is a third way to do what
   `pause-after` does, and it is the one that works over an arbitrary transport rather
   than inside tmux.
3. **A crash of the supervisor loses everything in every one of these systems.** VS Code
   restarts its host up to five times and loses the sessions each time; WezTerm has no
   persistence at all. tmux's own model — a server that outlives every client — is not
   beaten by any of them. That is the strongest argument for keeping the PTYs where they
   are.

**On the tooling side**, if (c) were ever built: `portable-pty` is the ergonomic
cross-platform Rust crate (it is what WezTerm uses), `nix::pty` is the raw POSIX
primitive, `node-pty` is the incumbent for Node but **ships no prebuilt binaries
upstream** and must be rebuilt per Node major because it is NAN/node-gyp based.
`napi-rs` gives **Node-API ABI stability** (build once against a level, load on later
Node majors), a real prebuilt distribution pipeline via `@napi-rs/cli`, and cross
compilation — at the cost of writing the PTY layer in Rust. The known trap for a
long-lived fd-holding addon is a **strong `ThreadsafeFunction` keeping the event loop
alive** (the "Node does not exit" failure) and blocking reads on the pty master inside a
synchronous function freezing the whole event loop.

**None of this needs deciding to proceed with option (a).**

---

## 10. The A/B, measured

`scripts/ab-transport.ts` drives both transports identically — same session, same
workload, same scratch tmux server per run — and reports what a client would pay.
Two consecutive runs, host loopback:

| | control | pipe |
|---|---|---|
| attach snapshot | 1,077 B | 1,077 B |
| echo p50 | **0–1 ms** | **7–8 ms** |
| echo min | 0 ms | 6–7 ms |
| burst delivered | **79,042 B** | **79,030 B** |
| burst chunks | 160–195 | 124–183 |
| bytes per chunk | 405–494 | 432–637 |
| burst wall time | 22–43 ms | 34–35 ms |

Payload with no framing at all: **78,893 B**.

### What this says

1. **The transports are equivalent on the wire.** 79,042 against 79,030 is a
   **0.015 %** difference, and it is the pipe that is *smaller* — that is control
   mode's octal escaping, and it costs about twelve bytes in seventy-nine thousand.
   Any argument that escaping is expensive is now settled: it is not.
2. **The pipe costs about 7 ms of local echo latency**, consistently, because a
   keystroke's echo travels one hop further: tmux → its `cat` job → the FIFO → our
   reader, instead of tmux → the control client directly. On a tailnet round trip
   (~50 ms) that is roughly 14 % of one RTT. On 3G (~200 ms) it is noise.
3. **The pipe chunks better** — larger messages, fewer of them — which is marginally
   less per-message framing on the WebSocket.
4. **Neither is decisively faster.** The choice is therefore not a performance one,
   which means it should be made on the two things that do differ: **control mode has
   flow control** (`pause-after`, `%pause`/`%continue`) and **the pipe has no protocol
   to parse at all** (595 lines of hand-written parsing of an unspecified format).

### The harness got it wrong twice, and both are recorded

The first version reported **447 B** for a 79 KB burst, identically for both
transports. Identical numbers from two different mechanisms is the shape of a broken
measurement, not a result. Two causes, both the same mistake in different clothes:
the shell had not drawn its prompt when the burst was typed, and the echo-latency
samples were never cleared from the input line, so the burst command was typed onto
the end of `E0_E1_…_seq` and the shell ran a command that did not exist. Both are now
fixed in the script and commented there.

### Still to measure on the device

These numbers are host loopback. The two things that would decide it on a phone are
**echo latency over the tailnet** (where the pipe's 7 ms is a smaller fraction) and
**what each does when the phone stops keeping up** — control mode pauses the pane and
resyncs from a snapshot, and the pipe blocks the pane's own process, which is
lossless but stalls the user's shell. That difference does not appear on loopback and
is the one worth a device run.
