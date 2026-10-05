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
                    if flag(rest, "--json").is_some() {
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
        "code" => {
            let r: Result<serde_json::Value, CallError> = daemon_call(&sock, "remote_info", &serde_json::json!({})).await;
            if let Err(e) = r {
                eprintln!("ldmux: {e}");
                std::process::exit(1);
            }
            let v = r.unwrap();
            let host = v.get("hostname").and_then(|t| t.as_str()).unwrap_or("");
            let port = v.get("port").and_then(|t| t.as_u64()).unwrap_or(0);
            let token = v.get("token").and_then(|t| t.as_str()).unwrap_or("");
            let link = format!("ldmux://{host}:{port}?token={token}");
            println!("{link}");
        }
        "pair" => {
            let r: Result<serde_json::Value, CallError> = daemon_call(&sock, "remote_info", &serde_json::json!({})).await;
            if let Err(e) = r {
                eprintln!("ldmux: {e}");
                std::process::exit(1);
            }
            let v = r.unwrap();
            let host = v.get("hostname").and_then(|t| t.as_str()).unwrap_or("");
            let port = v.get("port").and_then(|t| t.as_u64()).unwrap_or(0);
            let token = v.get("token").and_then(|t| t.as_str()).unwrap_or("");
            let link = format!("ldmux://{host}:{port}?token={token}");
            println!("{link}");
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
            // Create a shell and attach this terminal to it (the desk window of
            // plan §3.9), then wait. The attach client claims the shell.
            let pos = positional(rest, &[]);
            let kind = if cmd == "run" { pos.first().copied().unwrap_or("shell") } else { "shell" };
            let cwd_idx = if cmd == "run" { 1 } else { 0 };
            let cwd_default = std::env::current_dir().unwrap().to_string_lossy().into_owned();
            let cwd: &str = pos.get(cwd_idx).copied().unwrap_or(&cwd_default);
            let created: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "new",
                &serde_json::json!({ "kind": kind, "cwd": cwd, "createdBy": "local" }),
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
            let status = std::process::Command::new("ldmuxd").args(["attach", &id]).status();
            std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
        }
        "mirror" => {
            // A read-only view: the attach client with no identity, so it holds
            // nothing and cannot type. The argv comes from the desktop seam, which
            // is the one place that knows how a window is launched.
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let argv = ldmux_daemon::desktop::mirror_argv(id, &ldmux_daemon::desktop::terminal());
            let _ = std::process::Command::new(&argv[0]).args(&argv[1..]).spawn();
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
