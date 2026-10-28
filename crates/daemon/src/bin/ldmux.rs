//! The ldmux CLI. Talks to the daemon over its Unix socket.
//!
//! Port of `packages/daemon/src/cli.ts` (435 lines). The CLI is a separate
//! binary; it connects to the daemon's control socket (`ldmuxd.sock`) and
//! makes JSON RPC calls.
//!
//! ## Commands
//!
//! - `ping` — liveness check.
//! - `list` — list sessions.
//! - `new <shell|pi> <cwd> [--title T]` — create a session.
//! - `send <id> <text>` — send text.
//! - `key <id> <Key...>` — send keys.
//! - `capture <id> [--history]` — capture the screen.
//! - `snapshot <id>` — snapshot the session.
//! - `kill <id>` — kill a session.
//! - `close <id>` — close a session (unless a desktop window holds it).
//! - `forget <id>` — drop a record without touching the keeper.
//! - `adopt <keeper-name>` — adopt a keeper session.
//! - `remote` — the remote endpoint info.
//! - `code` — the pairing QR code.
//! - `pair` — the pairing link.
//! - `desktop` — the desktop inventory.
//! - `live <id> <workspace>` — make a session live.
//! - `unlive <id>` — close a live session.
//! - `clipboard-write on|off` — toggle clipboard write.
//! - `deskWindow`'s CLI forms: `window [cwd]` — open a new shell in a desk window
//!   (what `$mod+Return` runs); `run <shell|pi> [cwd]` and
//!   `shell [--name N] [--cwd DIR] [dir]` — become a desk window *in this
//!   terminal*, reusing a shell already in that directory unless `--name` asks
//!   for another.
//! - `mirror <id> [--workspace N] [--rw] [--terminal T]` — a view of a session's
//!   screen on the desktop, read-only unless `--rw`.

use ldmux_protocol::rpc::PingResult;
use ldmux_protocol::socket::{self as ps, CallError};

/// The daemon socket path, resolved exactly as the daemon resolves it.
///
/// This used to hand-roll the fallback (`$HOME/.ldmux/ldmuxd.sock`) instead of
/// calling the protocol helper, so the CLI looked somewhere the daemon never
/// binds — every command answered `connect: No such file or directory` unless
/// `LDMUX_STATE_DIR` happened to be exported. One resolver, one answer.
fn default_socket_path() -> String {
    ldmux_protocol::paths::resolve_socket_path()
}

/// A typed call against the daemon socket (the TS `daemonCall`).
async fn daemon_call<R: serde::Serialize, T: serde::de::DeserializeOwned>(
    sock: &str,
    method: &str,
    params: &R,
) -> Result<T, CallError> {
    ps::call(sock, method, params, 5000).await
}

/// This machine's name, for the pairing: what the app calls the host it is
/// talking to. `os.hostname()` in the TS, and the kernel's own file is where
/// that reads from.
fn machine_hostname() -> String {
    if let Ok(h) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let h = h.trim();
        if !h.is_empty() {
            return h.to_string();
        }
    }
    match std::env::var("HOSTNAME") {
        Ok(h) if !h.is_empty() => h,
        _ => "ldmux".to_string(),
    }
}

/// Report a failed daemon call and exit. A refused or missing socket means the
/// daemon is not running, which is the one failure with a one-line fix, so the fix
/// is printed with it.
fn die(e: &CallError, sock: &str) -> ! {
    eprintln!("ldmux: {e}");
    if e.0.starts_with("connect:") {
        eprintln!(
            "ldmux: the daemon is not answering on {sock}; start it with `systemctl --user start ldmux` (or run `ldmuxd`), \
             and check that LDMUX_STATE_DIR is the same for both"
        );
    }
    std::process::exit(1);
}

/// The directory this command runs in, as the daemon is told it. A deleted or
/// unreadable cwd is a user problem with a remedy, not a panic.
fn current_dir_or_exit() -> String {
    match std::env::current_dir() {
        Ok(d) => d.to_string_lossy().into_owned(),
        Err(e) => {
            eprintln!(
                "ldmux: cannot read the current directory ({e}); cd somewhere that exists or pass the directory as an argument"
            );
            std::process::exit(1);
        }
    }
}

/// One command's usage, for `ldmux <command> --help`.
fn command_help(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "ping" => {
            "ldmux ping\n  Ask the daemon whether it is alive; prints its version info as JSON."
        }
        "list" => {
            "ldmux list\n  List sessions: id, status, kind, cwd, title, keeper (and why a dead one ended)."
        }
        "desktop" => {
            "ldmux desktop [--json]\n  The desktop inventory: window manager, workspaces, windows."
        }
        "new" => {
            "ldmux new [shell|pi] [cwd] [--title T]\n  Create a session (default: a shell in the current directory); prints its id."
        }
        "shell" => {
            "ldmux shell [--name N] [--cwd DIR] [dir]\n  Become a desk window for a shell in this terminal. Reuses the shell already in that directory unless --name asks for another."
        }
        "run" => {
            "ldmux run [shell|pi] [cwd]\n  Create a session and become its desk window in this terminal."
        }
        "window" => {
            "ldmux window [cwd]\n  Create a shell and open it in a new desk window (the terminal is chosen by LDMUX_TERMINAL_CMD, LDMUX_TERMINAL, $TERMINAL, then known emulators)."
        }
        "send" => "ldmux send <id> <text...>\n  Type text into a session.",
        "speak" => {
            "ldmux speak <id> [--final] [--id <message-id>] [--stdin] [text...]\n  Send text for the phone to say aloud, to the clients viewing a session in voice mode. Without --stdin the text is the remaining arguments."
        }
        "key" => "ldmux key <id> <Key...>\n  Send named keys (Enter, Up, C-c, ...) to a session.",
        "capture" => {
            "ldmux capture <id> [--history]\n  Print the session's screen (with --history, its scrollback too)."
        }
        "snapshot" => "ldmux snapshot <id>\n  Print the session's full snapshot as JSON.",
        "kill" => "ldmux kill <id>\n  End a session and its shell.",
        "close" => {
            "ldmux close <id>\n  End a session, unless a desktop window holds it (then let go)."
        }
        "forget" => "ldmux forget <id>\n  Drop a session's record without touching the keeper.",
        "adopt" => "ldmux adopt <keeper-name>\n  Adopt a keeper session the daemon does not know.",
        "mirror" => {
            "ldmux mirror <id> [--workspace N] [--rw] [--terminal T]\n  A read-only view of a session in a new window (--rw lets it type). Placement on a workspace needs an i3/sway/hyprland adapter; without one the window opens where it opens."
        }
        "live" => {
            "ldmux live <id> [workspace]\n  Open a session in a desk window, optionally placed on a workspace."
        }
        "unlive" => "ldmux unlive <id>\n  Close a session's desk window.",
        "remote" => {
            "ldmux remote\n  The remote endpoint: host, port, token, and whether it is listening."
        }
        "mesh" => {
            "ldmux mesh [status] [--json]\n  The MeshMore link: state, the peer, the record budget left this hour, and the radio's own reading. Anything about frequencies lives on the gateway, not here."
        }
        "code" => {
            "ldmux code [--mesh]\n  Print a QR code the phone app scans to pair. --mesh adds the session key and the pairing secret for the mesh route (the code is then as sensitive as the token and the secret together)."
        }
        "pair" => {
            "ldmux pair [--mesh]\n  Print the pairing link. --mesh adds the mesh keys, which are secret."
        }
        "clipboard-write" => {
            "ldmux clipboard-write on|off\n  Allow or refuse terminal programs writing the clipboard."
        }
        _ => return None,
    })
}

/// The keys in a `mesh.pairing` answer, or `None` when either is not 64 hex characters.
fn mesh_keys_from_json(v: &serde_json::Value) -> Option<ldmux_protocol::pairing::MeshKeys> {
    fn key(v: &serde_json::Value, k: &str) -> Option<[u8; 32]> {
        let s = v.get(k)?.as_str()?;
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(out)
    }
    // hostRadio is optional, but if present it must be a key, not something that is dropped.
    let host_radio = match v.get("hostRadio") {
        None | Some(serde_json::Value::Null) => None,
        Some(_) => Some(key(v, "hostRadio")?),
    };
    Some(ldmux_protocol::pairing::MeshKeys {
        host_key: key(v, "hostKey")?,
        psk: key(v, "psk")?,
        host_radio,
    })
}

/// The human form of `mesh.status`.
///
/// Sentinels are printed as words, never as numbers: `-128` is not a weak
/// signal and `255` is not a full battery.
fn print_mesh_status(v: &serde_json::Value) {
    let text = |key: &str| v.get(key).and_then(|t| t.as_str()).map(str::to_owned);
    if let Some("disabled") = text("state").as_deref() {
        println!("mesh: no link configured on this host");
        println!(
            "\nset LDMUX_MESH_ADDR, LDMUX_MESH_TOKEN and LDMUX_MESH_PEER in the daemon's environment"
        );
        return;
    }
    println!(
        "state    {}",
        text("state").unwrap_or_else(|| "unknown".to_owned())
    );
    if let Some(addr) = text("addr") {
        println!("gateway  {addr}");
    }
    if let Some(peer) = text("peer") {
        println!("peer     {peer}");
    }
    match v.get("uplinkRecordMax").and_then(serde_json::Value::as_u64) {
        Some(capacity) => println!("capacity {capacity} B per record"),
        None => println!("capacity unknown (not bound yet)"),
    }
    match v.get("recordsLeft").and_then(serde_json::Value::as_u64) {
        Some(255) => println!("budget   255 or more records left this hour"),
        Some(left) => println!("budget   {left} records left this hour"),
        None => println!("budget   unknown (the gateway has not reported it)"),
    }
    let radio = match v.get("radioState").and_then(serde_json::Value::as_u64) {
        Some(0) => "connected",
        Some(1) => "disconnected",
        Some(2) => "radio lost",
        Some(3) => "no contact",
        _ => "unknown",
    };
    println!("radio    {radio}");
    let rssi = v.get("rssi").and_then(serde_json::Value::as_i64);
    let battery = v.get("batteryPercent").and_then(serde_json::Value::as_u64);
    let signal = match rssi {
        Some(-128) | None => "no reading".to_owned(),
        Some(dbm) => format!("{dbm} dBm"),
    };
    let charge = match battery {
        Some(255) | None => "not reported".to_owned(),
        Some(percent) => format!("{percent}%"),
    };
    println!("signal   {signal}, battery {charge}");
    if let Some(tenths) = v.get("txAirtimeTenths").and_then(serde_json::Value::as_u64) {
        println!(
            "airtime  {:.1} s this hour (gateway's own estimate)",
            tenths as f64 / 10.0
        );
    }
    if let Some(err) = text("lastError") {
        println!("last     {err}");
    }
    if let Some(reconnects) = v.get("reconnects").and_then(serde_json::Value::as_u64)
        && reconnects > 0
    {
        println!("retries  {reconnects}");
    }
    if let Some(session) = v.get("session") {
        print_mesh_session(session);
    }
}

/// The L3 session's lines. `off` is a one-liner; everything else is facts the session knows.
fn print_mesh_session(v: &serde_json::Value) {
    let text = |key: &str| v.get(key).and_then(|t| t.as_str()).map(str::to_owned);
    let number = |key: &str| v.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0);
    let state = text("state").unwrap_or_else(|| "unknown".to_owned());
    if state == "off" {
        println!("session  off (set LDMUX_MESH_PSK in the daemon's environment to enable it)");
        return;
    }
    println!("session  {state}");
    if let Some(key) = text("publicKey") {
        println!("  gateway key  {key}");
    }
    match text("peerKey") {
        Some(key) => println!("  phone key    {key} (pinned)"),
        None => println!("  phone key    none yet"),
    }
    println!(
        "  handshakes   {} accepted, {} refused",
        number("handshakes"),
        number("handshakesRefused")
    );
    println!(
        "  records      {} sent, {} retransmitted, {} dropped, {} pending",
        number("sent"),
        number("retransmits"),
        number("dropped"),
        number("pending")
    );
    println!("  received     {} bytes", number("receivedBytes"));
    if number("budgetStalls") > 0 {
        println!(
            "  held back    {} times by the airtime budget",
            number("budgetStalls")
        );
    }
    if let Some(err) = text("lastError") {
        println!("  last         {err}");
    }
}

/// Print the usage line.
fn print_usage() {
    eprintln!(
        "usage: ldmux <ping|list|desktop|new|shell|run|window|send|speak|key|capture|snapshot|kill|close|forget|adopt|mirror|live|unlive|remote|mesh|code|pair|clipboard-write> [args]"
    );
}

/// The first option no command knows, or None.
///
/// Every flag here is one a command actually reads. The list is closed on
/// purpose: a flag that is accepted and ignored is worse than an unknown one,
/// because the command runs and does something other than what was asked.
/// `--dry-run` was in this list in the TS and here, read by no command at all.
fn reject_unknown_flag(rest: &[String]) -> Option<String> {
    const KNOWN_FLAGS: &[&str] = &[
        "--cwd",
        "--history",
        "--json",
        "--mesh",
        "--name",
        "--rw",
        "--final",
        "--id",
        "--stdin",
        "--terminal",
        "--title",
        "--workspace",
    ];
    for arg in rest {
        if arg == "--" {
            return None;
        }
        if !arg.starts_with('-') || arg == "-" {
            continue;
        }
        let name = match arg.find('=') {
            Some(i) => &arg[..i],
            None => arg.as_str(),
        };
        if !KNOWN_FLAGS.contains(&name) {
            return Some(name.to_string());
        }
    }
    None
}

/// Extract the value of a `--flag <value>` pair.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a.as_str() == name)?;
    Some(args.get(i + 1)?.as_str())
}

/// Extract positional args (skipping `--flag` pairs and `--`).
fn positional<'a>(args: &'a [String], skip: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut end_of_options = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !end_of_options {
            if a == "--" {
                end_of_options = true;
                i += 1;
                continue;
            }
            if skip.contains(&a.as_str()) {
                i += 2;
                continue;
            }
            if a.starts_with("--") {
                i += 1;
                continue;
            }
        }
        out.push(a.as_str());
        i += 1;
    }
    out
}

#[tokio::main]
async fn main() {
    // Rust ignores SIGPIPE, so `ldmux list | head -1` made println! panic on EPIPE.
    // A CLI wants the Unix default: die quietly when the reader goes away. Set
    // before any output. Socket writes to the daemon use send(MSG_NOSIGNAL) and
    // still return EPIPE as an error. This is the CLI binary only; the daemon
    // keeps its own disposition.
    // SAFETY: installing SIG_DFL, no handler code runs.
    unsafe {
        let _ = nix::sys::signal::signal(
            nix::sys::signal::Signal::SIGPIPE,
            nix::sys::signal::SigHandler::SigDfl,
        );
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        std::process::exit(2);
    }
    let (cmd, rest) = (args[0].clone(), &args[1..]);

    // `--help` is a request about the tool, not a flag for a command.
    if cmd == "--help" || cmd == "-h" {
        print_usage();
        std::process::exit(0);
    }
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        match command_help(&cmd) {
            Some(h) => println!("{h}"),
            None => print_usage(),
        }
        std::process::exit(0);
    }
    if let Some(bad) = reject_unknown_flag(rest) {
        eprintln!("ldmux {cmd}: unknown option {bad}");
        print_usage();
        std::process::exit(2);
    }

    let sock = default_socket_path();

    match cmd.as_str() {
        "ping" => {
            let r: Result<PingResult, CallError> =
                daemon_call(&sock, "ping", &serde_json::json!({})).await;
            match r {
                Ok(r) => println!("{}", serde_json::to_string(&r).unwrap()),
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "list" => {
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "list", &serde_json::json!({})).await;
            match r {
                Ok(v) => {
                    let sessions = v
                        .get("sessions")
                        .and_then(|s| s.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if sessions.is_empty() {
                        println!("(no sessions)");
                    } else {
                        for s in &sessions {
                            let id = s.get("id").and_then(|t| t.as_str()).unwrap_or("");
                            let status = s.get("status").and_then(|t| t.as_str()).unwrap_or("");
                            let kind = s.get("kind").and_then(|t| t.as_str()).unwrap_or("");
                            let cwd = s.get("cwd").and_then(|t| t.as_str()).unwrap_or("");
                            let title = s.get("title").and_then(|t| t.as_str()).unwrap_or("");
                            let keeper_name =
                                s.get("keeperName").and_then(|t| t.as_str()).unwrap_or("");
                            let attach = if keeper_name.is_empty() {
                                "[rpc]".to_string()
                            } else {
                                format!("[{keeper_name}]")
                            };
                            // An ending says why, on the line, for the shells that
                            // ended: `dead` alone sends you looking for a cause.
                            // ... and how, when the shell's own ending is known: its
                            // exit status, or the signal that killed it.
                            let how = match (
                                s.get("exitCode").and_then(|t| t.as_i64()),
                                s.get("exitSignal").and_then(|t| t.as_i64()),
                            ) {
                                (Some(c), _) => format!(", exit {c}"),
                                (_, Some(sig)) => format!(", signal {sig}"),
                                _ => String::new(),
                            };
                            let why = match s.get("endedReason").and_then(|t| t.as_str()) {
                                Some(r) if status == "dead" => format!("  ({r}{how})"),
                                _ => String::new(),
                            };
                            println!("{id}  {status:7} {kind:5} {cwd}  \"{title}\"  {attach}{why}");
                        }
                    }
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "new" => {
            let pos = positional(rest, &["--title"]);
            let kind = pos.first().copied().unwrap_or("shell");
            let cwd_default = current_dir_or_exit();
            let cwd: &str = pos.get(1).copied().unwrap_or(&cwd_default);
            let title = flag(rest, "--title");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "new",
                &serde_json::json!({
                    "kind": kind,
                    "cwd": cwd,
                    "title": title,
                }),
            )
            .await;
            match r {
                Ok(v) => {
                    let id = v
                        .get("session")
                        .and_then(|s| s.get("id"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    println!("{id}");
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "send" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let text: String = pos.get(1..).unwrap_or(&[]).join(" ");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "send",
                &serde_json::json!({ "id": id, "text": text }),
            )
            .await;
            if let Err(e) = r {
                die(&e, &sock);
            }
        }
        "speak" => {
            let pos = positional(rest, &["--id"]);
            let id = pos.first().copied().unwrap_or("");
            let text: String = if rest.iter().any(|a| a == "--stdin") {
                let mut buf = String::new();
                if std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).is_err() {
                    eprintln!("ldmux speak: could not read standard input");
                    std::process::exit(1);
                }
                buf.trim_end().to_owned()
            } else {
                pos.get(1..).unwrap_or(&[]).join(" ")
            };
            let mut params = serde_json::json!({
                "sessionId": id,
                "text": text,
                "final": rest.iter().any(|a| a == "--final"),
            });
            if let Some(message_id) = flag(rest, "--id") {
                params["id"] = serde_json::json!(message_id);
            }
            match daemon_call::<serde_json::Value, serde_json::Value>(&sock, "speech", &params)
                .await
            {
                Ok(v) => {
                    let delivered = v
                        .get("delivered")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    println!("said to {delivered} client(s)");
                }
                Err(e) => die(&e, &sock),
            }
        }
        "key" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let keys: Vec<&str> = pos.get(1..).unwrap_or(&[]).to_vec();
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "send",
                &serde_json::json!({ "id": id, "keys": keys }),
            )
            .await;
            if let Err(e) = r {
                die(&e, &sock);
            }
        }
        "capture" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let history = rest.iter().any(|a| a == "--history");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "capture",
                &serde_json::json!({ "id": id, "history": history }),
            )
            .await;
            match r {
                Ok(v) => {
                    let screen = v.get("screen").and_then(|s| s.as_str()).unwrap_or("");
                    print!("{screen}");
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "snapshot" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "snapshot", &serde_json::json!({ "id": id })).await;
            match r {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "kill" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "kill", &serde_json::json!({ "id": id })).await;
            if let Err(e) = r {
                die(&e, &sock);
            }
            println!("killed {id}");
        }
        "close" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "close", &serde_json::json!({ "id": id })).await;
            match r {
                Ok(v) => {
                    let ended = v.get("ended").and_then(|t| t.as_bool()).unwrap_or(false);
                    if ended {
                        println!("closed {id}");
                    } else {
                        println!("let go of {id} (a desktop window holds it)");
                    }
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "forget" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "forget", &serde_json::json!({ "id": id })).await;
            match r {
                Ok(v) => {
                    let removed = v.get("removed").and_then(|t| t.as_bool()).unwrap_or(false);
                    if removed {
                        println!("forgot {id}");
                    } else {
                        println!("no such session {id}");
                    }
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "adopt" => {
            let pos = positional(rest, &[]);
            let keeper_name = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "adopt",
                &serde_json::json!({ "keeperName": keeper_name }),
            )
            .await;
            match r {
                Ok(v) => {
                    let id = v
                        .get("session")
                        .and_then(|s| s.get("id"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    let keeper_name = v
                        .get("session")
                        .and_then(|s| s.get("keeperName"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    println!("{id}  {keeper_name}");
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "remote" => {
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "remote_info", &serde_json::json!({})).await;
            match r {
                Ok(v) => {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap());
                    let listening = v
                        .get("listening")
                        .and_then(|t| t.as_bool())
                        .unwrap_or(false);
                    if listening {
                        let host = v.get("hostname").and_then(|t| t.as_str()).unwrap_or("");
                        let port = v.get("port").and_then(|t| t.as_u64()).unwrap_or(0);
                        let path = v.get("path").and_then(|t| t.as_str()).unwrap_or("");
                        println!("\nws://{host}:{port}{path}");
                    } else {
                        println!("\nnot listening yet");
                    }
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "mesh" => {
            let sub = rest.first().map(String::as_str).unwrap_or("status");
            if sub != "status" {
                eprintln!("ldmux mesh: unknown subcommand '{sub}'");
                eprintln!("usage: ldmux mesh status [--json]");
                std::process::exit(2);
            }
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "mesh.status", &serde_json::json!({})).await;
            match r {
                Ok(v) => {
                    if rest.iter().any(|a| a == "--json") {
                        println!("{}", serde_json::to_string_pretty(&v).unwrap());
                    } else {
                        print_mesh_status(&v);
                    }
                }
                Err(e) => die(&e, &sock),
            }
        }
        "desktop" => {
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "desktop", &serde_json::json!({})).await;
            match r {
                Ok(v) => {
                    if rest.iter().any(|a| a == "--json") {
                        println!("{}", serde_json::to_string_pretty(&v).unwrap());
                    } else {
                        let wm = v.get("wm").and_then(|t| t.as_str()).unwrap_or("unknown");
                        let detail = v.get("detail").and_then(|t| t.as_str()).unwrap_or("");
                        if detail.is_empty() {
                            println!("wm: {wm}");
                        } else {
                            println!("wm: {wm}  ({detail})");
                        }
                        if let Some(ws) = v.get("workspaces").and_then(|w| w.as_array()) {
                            for w in ws {
                                let focused =
                                    w.get("focused").and_then(|t| t.as_bool()).unwrap_or(false);
                                let name = w.get("name").and_then(|t| t.as_str()).unwrap_or("");
                                let count =
                                    w.get("windowCount").and_then(|t| t.as_u64()).unwrap_or(0);
                                let marker = if focused { "*" } else { " " };
                                let plural = if count == 1 { "" } else { "s" };
                                println!("  {marker} {name:12} {count} window{plural}");
                            }
                        }
                        if let Some(pi) = v.get("piSessions").and_then(|s| s.as_array())
                            && !pi.is_empty()
                        {
                            println!("pi sessions:");
                            for p in pi {
                                let label = p.get("label").and_then(|t| t.as_str()).unwrap_or("");
                                let pid = p.get("pid").and_then(|t| t.as_i64()).unwrap_or(0);
                                let cwd = p.get("cwd").and_then(|t| t.as_str()).unwrap_or("");
                                // Absent `workspace` is "no window"; present and
                                // empty is "a window whose workspace the WM did
                                // not name" — the two used to print the same
                                // words, so a pi filling a window read "no
                                // window", which is the anomaly phrase. And a
                                // pi linked to an ldmux session says which one:
                                // a desk window is not in the pi's ancestry, so
                                // its workspace is often unknown here while the
                                // session is not.
                                let ws = p.get("workspace").and_then(|t| t.as_str());
                                let linked = p.get("ldmuxSessionId").and_then(|t| t.as_str());
                                let where_str = match (ws, linked) {
                                    (Some(w), _) if !w.is_empty() => format!("ws {w}"),
                                    (Some(_), None) => "window".to_string(),
                                    (Some(_), Some(id)) => format!("session {id}"),
                                    (None, Some(id)) => format!("session {id}"),
                                    (None, None) => "no window".to_string(),
                                };
                                println!("  {label:18} {where_str:24} pid {pid}  {cwd}");
                            }
                        }
                    }
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "live" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let workspace = pos.get(1).copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "makeLive",
                &serde_json::json!({ "id": id, "workspace": workspace }),
            )
            .await;
            match r {
                Ok(v) => {
                    let ok = v.get("ok").and_then(|t| t.as_bool()).unwrap_or(false);
                    let detail = v.get("detail").and_then(|t| t.as_str()).unwrap_or("failed");
                    if ok {
                        println!("on the desktop ({workspace})");
                    } else {
                        println!("could not: {detail}");
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "unlive" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "closeLive", &serde_json::json!({ "id": id })).await;
            match r {
                Ok(v) => {
                    let ok = v.get("ok").and_then(|t| t.as_bool()).unwrap_or(false);
                    let detail = v.get("detail").and_then(|t| t.as_str()).unwrap_or("failed");
                    if ok {
                        println!("closed");
                    } else {
                        println!("could not: {detail}");
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "clipboard-write" => {
            let pos = positional(rest, &[]);
            let arg = pos.first().copied();
            if arg.map(|a| a != "on" && a != "off").unwrap_or(false) {
                eprintln!("usage: ldmux clipboard-write on|off");
                std::process::exit(2);
            }
            let on = arg == Some("on");
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "clipboardWrite", &serde_json::json!({ "on": on })).await;
            match r {
                Ok(v) => {
                    let on = v.get("on").and_then(|t| t.as_bool()).unwrap_or(false);
                    println!("clipboard-write {}", if on { "on" } else { "off" });
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "code" | "pair" => {
            // The pairing for the address this daemon actually listens on, and
            // the one place that knows how to validate it: a QR code the phone
            // can scan (`code`), or the same pairing as a link (`pair`). Both
            // refuse a daemon a phone cannot reach instead of printing a link
            // that goes nowhere (the TS rule, ADR-0016/ADR-0017).
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "remote_info", &serde_json::json!({})).await;
            let v = match r {
                Ok(v) => v,
                Err(e) => {
                    die(&e, &sock);
                }
            };
            let info = ldmux_daemon::pairing::ListenInfo {
                hostname: v
                    .get("hostname")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                port: v.get("port").and_then(|t| t.as_u64()).unwrap_or(0) as u16,
                token: v
                    .get("token")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                listening: v
                    .get("listening")
                    .and_then(|t| t.as_bool())
                    .unwrap_or(false),
            };
            let name = machine_hostname();
            // The mesh keys come from the daemon that holds them, never from this process's
            // environment, and only when asked for.
            let mesh_keys = if rest.iter().any(|a| a == "--mesh") {
                let r: Result<serde_json::Value, CallError> =
                    daemon_call(&sock, "mesh.pairing", &serde_json::json!({})).await;
                let v = match r {
                    Ok(v) => v,
                    Err(e) => {
                        die(&e, &sock);
                    }
                };
                match mesh_keys_from_json(&v) {
                    Some(k) => Some(k),
                    None => {
                        eprintln!(
                            "ldmux: the daemon's mesh.pairing answer was not two 64-hex keys"
                        );
                        std::process::exit(1);
                    }
                }
            } else {
                None
            };
            if cmd == "code" {
                match ldmux_daemon::pairing::code_text_with_mesh(&info, &name, mesh_keys.as_ref()) {
                    Ok(text) => print!("{text}"),
                    Err(problem) => {
                        eprintln!("ldmux: {}", problem.message(&info.hostname));
                        std::process::exit(1);
                    }
                }
            } else {
                match ldmux_daemon::pairing::pairing_for_with_mesh(&info, &name, mesh_keys.as_ref())
                {
                    Ok((_pairing, link)) => println!("{link}"),
                    Err(problem) => {
                        eprintln!("ldmux: {}", problem.message(&info.hostname));
                        std::process::exit(1);
                    }
                }
            }
        }
        "window" => {
            // Create a shell, then make it a desk window here (the daemon opens
            // the window with the attach client and claims it to the desktop).
            let pos = positional(rest, &[]);
            let cwd_default = current_dir_or_exit();
            let cwd: &str = pos.first().copied().unwrap_or(&cwd_default);
            let created: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "new",
                &serde_json::json!({ "kind": "shell", "cwd": cwd, "createdBy": "local" }),
            )
            .await;
            let id = match created {
                Ok(v) => v
                    .get("session")
                    .and_then(|s| s.get("id"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                Err(e) => {
                    die(&e, &sock);
                }
            };
            let res: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "deskWindow", &serde_json::json!({ "id": id })).await;
            match res {
                Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {}
                Ok(v) => {
                    eprintln!(
                        "ldmux: {}",
                        v.get("detail")
                            .and_then(|x| x.as_str())
                            .unwrap_or("could not make a desk window")
                    );
                    std::process::exit(1);
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "run" | "shell" => {
            // `run <kind> [cwd]` creates a shell and becomes its desk window in
            // this terminal. `shell [--name N] [--cwd DIR] [dir]` is the same
            // for a shell, except that it reuses one already in that directory:
            // running `ldmux shell` twice in one place is "get me back to it",
            // not "give me a second one". The directory is the identity — a
            // title is not stable, and matching on one made the whole rule dead
            // once the phone titled a shell by its process name — so `--name` is
            // the escape hatch for wanting a second one on purpose.
            //
            // This terminal *is* the window, so the daemon is told the window is
            // the attach client's process (`spawn: false`, and the pid). Without
            // that the daemon knows nothing about the window: the shell would
            // report "no window" while it filled one, and closing the terminal
            // would leave the shell running with nothing left to end it.
            let pos = positional(rest, &["--name", "--cwd"]);
            let (kind, positional_cwd) = if cmd == "run" {
                (
                    pos.first().copied().unwrap_or("shell").to_string(),
                    pos.get(1).copied().map(|s| s.to_string()),
                )
            } else {
                (
                    "shell".to_string(),
                    pos.first().copied().map(|s| s.to_string()),
                )
            };
            let cwd_default = current_dir_or_exit();
            let want_cwd: String = flag(rest, "--cwd")
                .map(|s| s.to_string())
                .or(positional_cwd)
                .unwrap_or(cwd_default);
            let name = flag(rest, "--name").map(|s| s.to_string());

            let mut reused: Option<String> = None;
            if cmd == "shell" {
                let listed: Result<serde_json::Value, CallError> =
                    daemon_call(&sock, "list", &serde_json::json!({})).await;
                match listed {
                    Ok(v) => {
                        let empty = Vec::new();
                        let sessions = v
                            .get("sessions")
                            .and_then(|s| s.as_array())
                            .unwrap_or(&empty);
                        for s in sessions {
                            let is_shell = s.get("kind").and_then(|t| t.as_str()) == Some("shell");
                            let alive = s.get("status").and_then(|t| t.as_str()) != Some("dead");
                            let backed = s
                                .get("keeperName")
                                .and_then(|t| t.as_str())
                                .map(|k| !k.is_empty())
                                .unwrap_or(false);
                            if !(is_shell && alive && backed) {
                                continue;
                            }
                            let matches = match &name {
                                Some(n) => {
                                    s.get("title").and_then(|t| t.as_str()) == Some(n.as_str())
                                }
                                None => {
                                    s.get("cwd").and_then(|t| t.as_str()) == Some(want_cwd.as_str())
                                }
                            };
                            if matches {
                                reused =
                                    s.get("id").and_then(|t| t.as_str()).map(|x| x.to_string());
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        die(&e, &sock);
                    }
                }
            }
            let id: String = match reused {
                Some(id) => id,
                None => {
                    let created: Result<serde_json::Value, CallError> = daemon_call(
                        &sock,
                        "new",
                        &serde_json::json!({
                            "kind": kind,
                            "cwd": want_cwd,
                            "title": name,
                            "createdBy": "local",
                        }),
                    )
                    .await;
                    match created {
                        Ok(v) => v
                            .get("session")
                            .and_then(|s| s.get("id"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                        Err(e) => {
                            die(&e, &sock);
                        }
                    }
                }
            };

            // The attach client is the window, so it starts first and its pid is
            // what the daemon records. An absolute binary: a bare `ldmuxd` is
            // whatever `PATH` happens to hold.
            let mut child = match std::process::Command::new(ldmux_daemon::desktop::attach_bin())
                .args(["attach", &id])
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("ldmux: could not start the attach client: {e}");
                    std::process::exit(1);
                }
            };
            let reg: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "deskWindow",
                &serde_json::json!({ "id": id, "spawn": false, "pid": child.id() as i32 }),
            )
            .await;
            let registered =
                matches!(&reg, Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false));
            if !registered {
                let _ = child.kill();
                let _ = child.wait();
                match reg {
                    Ok(v) => eprintln!(
                        "ldmux: {}",
                        v.get("detail")
                            .and_then(|x| x.as_str())
                            .unwrap_or("could not make a desk window")
                    ),
                    Err(e) => eprintln!("ldmux: {e}"),
                }
                std::process::exit(1);
            }
            let status = child.wait();
            std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
        }
        "mirror" => {
            // A view of a shell's screen on a chosen workspace, in a chosen
            // terminal, read-only unless `--rw`. The argv comes from the desktop
            // seam, the one place that knows how a window is launched, and the
            // placement is the same exec-then-move the desk window uses — the TS
            // mirror ran `workspace number N` first, which dragged the user to
            // workspace 10.
            let pos = positional(rest, &["--workspace", "--terminal"]);
            let id = pos.first().copied().unwrap_or("");
            let listed: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "list", &serde_json::json!({})).await;
            let keeper = match listed {
                Ok(v) => v
                    .get("sessions")
                    .and_then(|s| s.as_array())
                    .and_then(|a| {
                        a.iter()
                            .find(|s| s.get("id").and_then(|t| t.as_str()) == Some(id))
                    })
                    .and_then(|s| s.get("keeperName"))
                    .and_then(|t| t.as_str())
                    .filter(|k| !k.is_empty())
                    .map(|k| k.to_string()),
                Err(e) => {
                    die(&e, &sock);
                }
            };
            let Some(keeper) = keeper else {
                eprintln!("ldmux: no terminal-backed session {id}");
                std::process::exit(1);
            };
            let workspace = flag(rest, "--workspace")
                .map(|w| w.to_string())
                .unwrap_or_else(|| ldmux_daemon::desktop::DEFAULT_MIRROR_WORKSPACE.to_string());
            let terminal_name = flag(rest, "--terminal")
                .map(|t| t.to_string())
                .unwrap_or_else(ldmux_daemon::desktop::terminal);
            // `i3-msg exec` says success for a command it merely queued, so a
            // mistyped terminal would otherwise be a window that dies in the same
            // instant, with nothing said.
            if let Some(problem) = ldmux_daemon::desktop::terminal_problem(&terminal_name) {
                eprintln!("ldmux: {problem}");
                std::process::exit(1);
            }
            let read_only = !rest.iter().any(|a| a == "--rw");
            match ldmux_daemon::desktop::open_mirror(id, &workspace, read_only, &terminal_name)
                .await
            {
                Ok(lr) if lr.ok => println!("mirror of {keeper} on workspace {workspace}"),
                Ok(lr) => {
                    eprintln!(
                        "ldmux: mirror not opened ({})",
                        lr.detail.unwrap_or_else(|| "failed".to_string())
                    );
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("ldmux: mirror not opened ({e})");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("ldmux: unknown command {cmd}");
            print_usage();
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod mesh_keys_tests {
    use super::mesh_keys_from_json;
    use serde_json::json;

    #[test]
    fn two_64_hex_keys_parse_and_anything_else_does_not() {
        let k = "ab".repeat(32);
        let ok = mesh_keys_from_json(&json!({"hostKey": k, "psk": k})).unwrap();
        assert_eq!(ok.host_key, [0xab; 32]);
        assert!(mesh_keys_from_json(&json!({"hostKey": k})).is_none());
        assert!(mesh_keys_from_json(&json!({"hostKey": k, "psk": "ab"})).is_none());
        assert!(mesh_keys_from_json(&json!({"hostKey": k, "psk": "zz".repeat(32)})).is_none());
        let with =
            mesh_keys_from_json(&json!({"hostKey": k, "psk": k, "hostRadio": "cd".repeat(32)}))
                .unwrap();
        assert_eq!(with.host_radio, Some([0xcd; 32]));
        assert_eq!(
            mesh_keys_from_json(&json!({"hostKey": k, "psk": k, "hostRadio": null}))
                .unwrap()
                .host_radio,
            None
        );
        assert!(mesh_keys_from_json(&json!({"hostKey": k, "psk": k, "hostRadio": "ab"})).is_none());
    }
}

#[cfg(test)]
mod socket_path_tests {
    /// The CLI must not resolve the socket itself: a hand-rolled fallback sent it
    /// to a path the daemon never binds, and every command failed with
    /// `connect: No such file or directory` unless `LDMUX_STATE_DIR` was exported.
    #[test]
    fn cli_socket_path_is_the_protocol_one() {
        assert_eq!(
            super::default_socket_path(),
            ldmux_protocol::paths::resolve_socket_path()
        );
        assert!(
            super::default_socket_path().ends_with("ldmuxd.sock"),
            "one resolver, one answer"
        );
    }
}
