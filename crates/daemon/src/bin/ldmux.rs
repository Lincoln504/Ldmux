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
//! - `window [cwd]` — open a new shell in a desk window.
//! - `run <shell|pi> [cwd]` / `shell` — run a session in this terminal (attach).
//! - `mirror <id>` — a read-only desk window onto a session.

use ldmux_protocol::socket::{self as ps, CallError};
use ldmux_protocol::rpc::PingResult;

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

/// Print the usage line.
fn print_usage() {
    eprintln!(
        "usage: ldmux <ping|list|desktop|new|shell|run|window|send|key|capture|snapshot|kill|close|forget|adopt|mirror|live|unlive|remote|code|pair|clipboard-write> [args]"
    );
}

/// The first option no command knows, or None.
fn reject_unknown_flag(rest: &[String]) -> Option<String> {
    const KNOWN_FLAGS: &[&str] = &[
        "--cwd", "--dry-run", "--history", "--json", "--name",
        "--rw", "--terminal", "--title", "--workspace",
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
    if let Some(bad) = reject_unknown_flag(rest) {
        if bad == "--help" || bad == "-h" {
            print_usage();
            std::process::exit(0);
        }
        eprintln!("ldmux {cmd}: unknown option {bad}");
        print_usage();
        std::process::exit(2);
    }

    let sock = default_socket_path();

    match cmd.as_str() {
        "ping" => {
            let r: Result<PingResult, CallError> = daemon_call(&sock, "ping", &serde_json::json!({})).await;
            match r {
                Ok(r) => println!("{}", serde_json::to_string(&r).unwrap()),
                Err(e) => {
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            }
        }
        "list" => {
            let r: Result<serde_json::Value, CallError> = daemon_call(&sock, "list", &serde_json::json!({})).await;
            match r {
                Ok(v) => {
                    let sessions = v.get("sessions").and_then(|s| s.as_array()).cloned().unwrap_or_default();
                    if sessions.is_empty() {
                        println!("(no sessions)");
                    } else {
                        for s in &sessions {
                            let id = s.get("id").and_then(|t| t.as_str()).unwrap_or("");
                            let status = s.get("status").and_then(|t| t.as_str()).unwrap_or("");
                            let kind = s.get("kind").and_then(|t| t.as_str()).unwrap_or("");
                            let cwd = s.get("cwd").and_then(|t| t.as_str()).unwrap_or("");
                            let title = s.get("title").and_then(|t| t.as_str()).unwrap_or("");
                            let keeper_name = s.get("keeperName").and_then(|t| t.as_str()).unwrap_or("");
                            let attach = if keeper_name.is_empty() { "[rpc]".to_string() } else { format!("[{keeper_name}]") };
                            println!("{id}  {status:7} {kind:5} {cwd}  \"{title}\"  {attach}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            }
        }
        "new" => {
            let pos = positional(rest, &["--title"]);
            let kind = pos.first().copied().unwrap_or("shell");
            let cwd_default = std::env::current_dir().unwrap().to_string_lossy().into_owned();
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
                    let id = v.get("session").and_then(|s| s.get("id")).and_then(|t| t.as_str()).unwrap_or("");
                    println!("{id}");
                }
                Err(e) => {
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
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
                eprintln!("ldmux: {e}");
                std::process::exit(1);
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
                eprintln!("ldmux: {e}");
                std::process::exit(1);
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
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            }
        }
        "snapshot" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "snapshot",
                &serde_json::json!({ "id": id }),
            )
            .await;
            match r {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
                Err(e) => {
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            }
        }
        "kill" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "kill",
                &serde_json::json!({ "id": id }),
            )
            .await;
            if let Err(e) = r {
                eprintln!("ldmux: {e}");
                std::process::exit(1);
            }
            println!("killed {id}");
        }
        "close" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "close",
                &serde_json::json!({ "id": id }),
            )
            .await;
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
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            }
        }
        "forget" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "forget",
                &serde_json::json!({ "id": id }),
            )
            .await;
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
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
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
                    let id = v.get("session").and_then(|s| s.get("id")).and_then(|t| t.as_str()).unwrap_or("");
                    let keeper_name = v.get("session").and_then(|s| s.get("keeperName")).and_then(|t| t.as_str()).unwrap_or("");
                    println!("{id}  {keeper_name}");
                }
                Err(e) => {
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            }
        }
        "remote" => {
            let r: Result<serde_json::Value, CallError> = daemon_call(&sock, "remote_info", &serde_json::json!({})).await;
            match r {
                Ok(v) => {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap());
                    let listening = v.get("listening").and_then(|t| t.as_bool()).unwrap_or(false);
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
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            }
        }
        "desktop" => {
            let r: Result<serde_json::Value, CallError> = daemon_call(&sock, "desktop", &serde_json::json!({})).await;
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
                                let focused = w.get("focused").and_then(|t| t.as_bool()).unwrap_or(false);
                                let name = w.get("name").and_then(|t| t.as_str()).unwrap_or("");
                                let count = w.get("windowCount").and_then(|t| t.as_u64()).unwrap_or(0);
                                let marker = if focused { "*" } else { " " };
                                let plural = if count == 1 { "" } else { "s" };
                                println!("  {marker} {name:12} {count} window{plural}");
                            }
                        }
                        if let Some(pi) = v.get("piSessions").and_then(|s| s.as_array())
                            && !pi.is_empty() {
                                println!("pi sessions:");
                                for p in pi {
                                    let label = p.get("label").and_then(|t| t.as_str()).unwrap_or("");
                                    let ws = p.get("workspace").and_then(|t| t.as_str()).unwrap_or("");
                                    let where_str = if ws.is_empty() { "no window" } else { &format!("ws {ws}") };
                                    let pid = p.get("pid").and_then(|t| t.as_i64()).unwrap_or(0);
                                    let cwd = p.get("cwd").and_then(|t| t.as_str()).unwrap_or("");
                                    println!("  {label:18} {where_str:10} pid {pid}  {cwd}");
                                }
                            }
                    }
                }
                Err(e) => {
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
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
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            }
        }
        "unlive" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "closeLive",
                &serde_json::json!({ "id": id }),
            )
            .await;
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
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
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
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "clipboardWrite",
                &serde_json::json!({ "on": on }),
            )
            .await;
            match r {
                Ok(v) => {
                    let on = v.get("on").and_then(|t| t.as_bool()).unwrap_or(false);
                    println!("clipboard-write {}", if on { "on" } else { "off" });
                }
                Err(e) => {
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            }
        }
        "code" | "pair" => {
            // The pairing for the address this daemon actually listens on, and
            // the one place that knows how to validate it: a QR code the phone
            // can scan (`code`), or the same pairing as a link (`pair`). Both
            // refuse a daemon a phone cannot reach instead of printing a link
            // that goes nowhere (the TS rule, ADR-0016/ADR-0017).
            let r: Result<serde_json::Value, CallError> = daemon_call(&sock, "remote_info", &serde_json::json!({})).await;
            let v = match r {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            };
            let info = ldmux_daemon::pairing::ListenInfo {
                hostname: v.get("hostname").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                port: v.get("port").and_then(|t| t.as_u64()).unwrap_or(0) as u16,
                token: v.get("token").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                listening: v.get("listening").and_then(|t| t.as_bool()).unwrap_or(false),
            };
            let name = machine_hostname();
            if cmd == "code" {
                match ldmux_daemon::pairing::code_text(&info, &name) {
                    Ok(text) => print!("{text}"),
                    Err(problem) => {
                        eprintln!("ldmux: {}", problem.message(&info.hostname));
                        std::process::exit(1);
                    }
                }
            } else {
                match ldmux_daemon::pairing::pairing_for(&info, &name) {
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
            let cwd_default = std::env::current_dir().unwrap().to_string_lossy().into_owned();
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
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
                }
            };
            let res: Result<serde_json::Value, CallError> =
                daemon_call(&sock, "deskWindow", &serde_json::json!({ "id": id })).await;
            match res {
                Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {}
                Ok(v) => {
                    eprintln!(
                        "ldmux: {}",
                        v.get("detail").and_then(|x| x.as_str()).unwrap_or("could not make a desk window")
                    );
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
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
                ("shell".to_string(), pos.first().copied().map(|s| s.to_string()))
            };
            let cwd_default = std::env::current_dir().unwrap().to_string_lossy().into_owned();
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
                        let sessions = v.get("sessions").and_then(|s| s.as_array()).unwrap_or(&empty);
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
                                reused = s.get("id").and_then(|t| t.as_str()).map(|x| x.to_string());
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("ldmux: {e}");
                        std::process::exit(1);
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
                            eprintln!("ldmux: {e}");
                            std::process::exit(1);
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
                    eprintln!("ldmux: {e}");
                    std::process::exit(1);
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
            let read_only = !rest.iter().any(|a| a == "--rw");
            match ldmux_daemon::desktop::open_mirror(id, &workspace, read_only, &terminal_name).await {
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
