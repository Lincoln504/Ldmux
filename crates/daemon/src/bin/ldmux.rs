//! The ldmux CLI. Talks to the daemon over its Unix socket.
//!
//! Port of `packages/daemon/src/cli.ts` (435 lines). The CLI is a separate
//! binary; it connects to the daemon's control socket (`ldmuxd.sock`) and
//! makes JSON RPC calls.
//!
//! ## Commands
//!
//! The shell's own lifecycle, in this terminal:
//!
//! - `new [--cwd DIR] [dir]` — create a shell and print its id, attaching to nothing.
//! - `shell [--name N] [--cwd DIR] [dir]`, `run [cwd]` — become a desk window *in this
//!   terminal*, reusing a shell already in that directory unless `--name` asks for another.
//! - `attach <id|prefix> [--readonly]` — an existing shell in this terminal.
//! - `list [--all] [--json]` — the live sessions.
//! - `kill <id>`, `close <id>` — end a session; `close` lets go instead when a desk window
//!   holds it.
//! - `forget <id>`, `adopt <keeper-name>` — repair: drop a dead record, or take back a keeper
//!   session the registry lost.
//!
//! Driving one from a script: `send <id> <text>`, `key <id> <Key...>`,
//! `capture <id> [--history]`.
//!
//! The host itself: `ping`, `terminal [--explain] [--exec <command…>] | set | clear`
//! (which emulator a spawned window opens in, and why), `mesh [status|preset]`, `code`,
//! `pair`, `clipboard-write on|off`.
//!
//! There is no verb that opens a window. The one window the daemon opens is the phone's
//! (`send to desktop`); a person at a keyboard is already in the terminal they want.

use ldmux_protocol::rpc::{PingResult, method};
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
    // Above the daemon's own worst case (`socket::DEFAULT_TIMEOUT_MS` bounds the keeper call at
    // 8 s), so a slow-but-successful call is not reported to the user as a timeout. A client
    // that gives up first turns a success into a failure the user cannot distinguish from a
    // real one.
    ps::call(sock, method, params, 15_000).await
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
            "ldmux ping\n  Ask the daemon whether it is alive; prints its protocol version, its build, and its keeper socket as JSON. A daemon too old to know `build` omits it."
        }
        "list" => {
            "ldmux list [--all] [--json]\n  List the live sessions: id, status, cwd, title, keeper. --all includes the shells that ended, with why; --json is unchanged and always carries every record."
        }
        "shell" => {
            "ldmux shell [--name N] [--cwd DIR] [dir]\n  Become a desk window for a shell in this terminal. Reuses the shell already in that directory unless --name asks for another."
        }
        "attach" => {
            "ldmux attach <id|prefix> [--readonly]\n  Attach to an existing shell in this terminal. The desk client is this terminal: no new window is opened."
        }
        "new" => {
            "ldmux new [cwd]\n  Create a shell and print its id, without attaching to it or opening anything. Look at it with `ldmux attach <id>`."
        }
        "terminal" => {
            "ldmux terminal [--explain] [--exec <command...>]\n  The terminal a desk window opens in, and where the answer came from (--explain). The order: a pin set here, $TERMINAL, xdg-terminal-exec, this desktop session's configured terminal, an emulator already running, the system alternative, then the known list.\n\nldmux terminal set <program>\n  Pin it, for example `ldmux terminal set alacritty`. Kept in $XDG_CONFIG_HOME/ldmux/terminal, which the daemon reads.\n\nldmux terminal clear\n  Forget the pin.\n\nldmux terminal --exec <command...>\n  Run <command> in that terminal (the emulator's own flags applied)."
        }
        "buzz" => {
            "ldmux buzz [--kind attention|done] [--note <text>] [--session <id>]\n  Mark a shell in the phone's list: `done` when an agent finished a turn, `attention` when it needs a look. Run inside a shell and the shell names itself ($LDMUX_SESSION_ID); from outside, pass --session. Refused with one sentence and a non-zero exit for a shell that is not running, a bad kind, or a repeat inside 5 s; never retried, because it is a mark, not a control."
        }
        "title" => {
            "ldmux title <words...>\n  Name this shell after the conversation in it: one to three words, 24 bytes at most. The phone's row and `ldmux list` show that name instead of the folder. The shell names itself ($LDMUX_SESSION_ID); from outside, pass --session <id>. Naming a pi or Claude Code session also sets that agent's own session name, when the shell is running one."
        }
        "send" => "ldmux send <id> <text...>\n  Type text into a session.",
        "key" => "ldmux key <id> <Key...>\n  Send named keys (Enter, Up, C-c, ...) to a session.",
        "capture" => {
            "ldmux capture <id> [--history]\n  Print the session's screen (with --history, its scrollback too)."
        }
        "kill" => "ldmux kill <id>\n  End a session and its shell.",
        "close" => {
            "ldmux close <id>\n  End a session, unless a desktop window holds it (then let go)."
        }
        "forget" => "ldmux forget <id>\n  Drop a session's record without touching the keeper.",
        "adopt" => "ldmux adopt <keeper-name>\n  Adopt a keeper session the daemon does not know.",
        "mesh" => {
            "ldmux mesh [status|preset ...] [--json]\n  The radio route: the device the daemon owns, the records left this hour by the radio's own count, its reading, and the tuple it is tuned to.\n\nldmux mesh preset <MHz> <kHz> <SF> <CR> [--path 1|2|3]\n  Tune the host's own radio, e.g. `ldmux mesh preset 910.525 62.5 7 5`. The firmware validates it and the command fails if it refuses. `--path` is the path hash width in bytes, and it is part of the channel whenever other nodes are in the path: MeshCore's public presets are one byte, ldmux's own channel is two."
        }
        "code" => {
            "ldmux code [--mesh | --mesh-only]\n  Print a QR code the phone app scans to pair. --mesh adds the session key and the pairing secret for the mesh route (the code is then as sensitive as the token and the secret together). --mesh-only carries the mesh keys and nothing of the tailnet: no address, no daemon token, for a phone that will only ever reach this machine by radio."
        }
        "pair" => {
            "ldmux pair [--mesh | --mesh-only]\n  Print the pairing link. --mesh adds the mesh keys, which are secret. --mesh-only prints a link with the mesh keys and no tailnet address or token."
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
    // The host's tuple, when the daemon reports one: four numbers and, when the daemon was asked
    // for it, a fifth that is the path hash width. A shape this CLI will not guess at is left out
    // rather than sent as a wrong frequency.
    let host_preset = v.get("hostPreset").and_then(|p| p.as_array()).and_then(|p| {
        if p.len() != 4 && p.len() != 5 {
            return None;
        }
        Some(ldmux_protocol::pairing::HostPreset {
            freq_khz: u32::try_from(p[0].as_u64()?).ok()?,
            bw_hz: u32::try_from(p[1].as_u64()?).ok()?,
            sf: u8::try_from(p[2].as_u64()?).ok()?,
            cr: u8::try_from(p[3].as_u64()?).ok()?,
            path_bytes: p.get(4).and_then(|v| v.as_u64()).map(|v| v as u8),
        })
    });
    Some(ldmux_protocol::pairing::MeshKeys {
        host_preset,
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
        println!("mesh: no radio route on this host");
        println!(
            "\nset LDMUX_MESH_SERIAL to the host radio's device path (the daemon owns the radio),"
        );
        println!("and LDMUX_MESH_PSK to the pairing's secret, in the daemon's environment");
        return;
    }
    println!(
        "state    {}",
        text("state").unwrap_or_else(|| "unknown".to_owned())
    );
    // The device the daemon owns. There is no address and no peer: the pairing's other end is
    // whichever phone holds the keys. Printing an empty `gateway` line and `peer 00000000` for
    // that was two facts that do not exist.
    let serial = text("serial").filter(|s| !s.is_empty());
    if let Some(device) = &serial {
        println!("serial   {device}");
    }
    // Printed before the radio's own state, because it is the answer to "can I hand a phone
    // a code at all": a code with no host radio sends it to `NeedsPeer`, and scanning the
    // same code again cannot help — the key is not in it.
    // `host radio`, not `radio`: the link's state line below is also about the radio, and one
    // label for two facts is how an operator reads the key as the state.
    match text("hostRadio") {
        Some(radio) => println!("host radio {radio}"),
        None => {
            println!("host radio not set — a pairing code cannot be used without it");
            println!("         open the radio (set LDMUX_MESH_SERIAL) so the daemon can read it,");
            println!("         or set LDMUX_MESH_SELF to pin its key, then print the code again");
        }
    }
    // **The tuple the host's radio is on, because the phone's must match it.** Two radios on
    // different tuples cannot hear each other, and until this line existed the host never said
    // what its own was tuned to: the failure looked exactly like being out of range.
    if let Some(p) = v.get("radioPreset").and_then(|p| p.as_array())
        && p.len() == 4
    {
        let n = |i: usize| p[i].as_u64().unwrap_or(0);
        // **The path width is part of the tuple on this radio.** A wider hash is what keeps two
        // repeaters from colliding; it is also what two ends must agree about to read each other's
        // hops, so an operator comparing this line with the phone's `path N B` row compares the
        // whole tuple rather than three quarters of it.
        let width = v
            .get("pathHashSize")
            .and_then(serde_json::Value::as_u64)
            .map(|w| format!("  path {w} B"))
            .unwrap_or_default();
        // **Power is the third thing two radios can disagree about.** The phone shows its own, so a
        // host that stays silent leaves an operator comparing half a channel: same tuple, same
        // width, and one end shouting at 22 dBm while the other whispers at 14.
        let power = v
            .get("txPowerDbm")
            .and_then(serde_json::Value::as_i64)
            .map(|p| format!("  {p} dBm"))
            .unwrap_or_default();
        println!(
            "preset   {:.3} MHz  {:.1} kHz  SF{}  CR{}{width}{power}",
            n(0) as f64 / 1000.0,
            n(1) as f64 / 1000.0,
            n(2),
            n(3)
        );
    }
    match v.get("uplinkRecordMax").and_then(serde_json::Value::as_u64) {
        // The carrier's record, not the terminal protocol's payload: a `Rows` message holds at
        // most 143 application bytes inside it, and an operator reading "172" beside a protocol
        // figure needs the two labelled.
        Some(capacity) => println!("capacity {capacity} B per carrier record"),
        None => println!("capacity unknown (not bound yet)"),
    }
    // **Not a limit this host enforces.** The radio's firmware runs the transmitter's duty cycle,
    // and what it reports is its own count: printing it as a budget read like a gate the daemon
    // was applying, which it is not.
    match v.get("recordsLeft").and_then(serde_json::Value::as_u64) {
        Some(255) => println!("records  up to 255 left this hour (the field's ceiling)"),
        Some(left) => println!("records  {left} left this hour, by the radio's own count"),
        None => println!("records  not reported yet"),
    }
    let radio = match v.get("radioState").and_then(serde_json::Value::as_u64) {
        Some(0) => "connected",
        Some(1) => "disconnected",
        Some(2) => "radio lost",
        Some(3) => "no contact",
        _ => "unknown",
    };
    println!("link     {radio}");
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
            "airtime  {:.1} s this hour (the radio's own estimate)",
            f64::from(tenths as u32) / 10.0
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
        // One name for one fact: this is the session's static key, which the phone pins.
        println!("  session key  {key}");
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
        "usage: ldmux <ping|list|new|shell|attach|terminal|send|key|capture|buzz|title|kill|close|forget|adopt|mesh|code|pair|clipboard-write> [args]"
    );
    eprintln!("       ldmux --version    which build this is");
}

/// The `mesh` subcommand: `status` unless a name is given.
///
/// A leading flag is a flag, not a subcommand. `ldmux mesh --json` used to be answered with
/// "unknown subcommand '--json'" even though the usage line documents `ldmux mesh [status]
/// [--json]`, so the documented form only worked when `status` was spelled out.
fn mesh_subcommand(rest: &[String]) -> &str {
    match rest.first().map(String::as_str) {
        Some(a) if a.starts_with('-') => "status",
        Some(a) => a,
        None => "status",
    }
}

/// The four numbers of a channel, and optionally its path hash width.
///
/// `--path` is separate from the tuple because the firmware keeps them separately, but they are
/// one channel to a network: MeshCore's public presets name neighbours in one byte, our own in
/// two, and a node whose width disagrees with the repeater in between cannot read the path that
/// repeater built. The width is in bytes, because that is what `ldmux mesh status` prints.
/// A retune as the CLI parses it: the four numbers, then the two registers that are not part of
/// the channel — the path width and the power, either of which may be left alone.
type PresetArgs = (f64, f64, u8, u8, Option<u8>, Option<i8>);

fn preset_args(n: &[&str]) -> Result<PresetArgs, String> {
    let mut positional: Vec<&str> = Vec::new();
    let mut path: Option<u8> = None;
    let mut power: Option<i8> = None;
    let mut it = n.iter().copied();
    while let Some(a) = it.next() {
        if a == "--power" {
            let Some(v) = it.next() else {
                return Err("--power needs a number of dBm: -9 to 30".to_string());
            };
            let parsed = v
                .parse::<i8>()
                .map_err(|_| format!("--power is dBm (-9 to 30), not {v:?}"))?;
            if !(-9..=30).contains(&parsed) {
                return Err(format!("--power is -9 to 30 dBm, not {parsed}"));
            }
            power = Some(parsed);
        } else if a == "--path" {
            let Some(v) = it.next() else {
                return Err("--path needs a number: 1, 2 or 3".to_string());
            };
            let parsed = v
                .parse::<u8>()
                .map_err(|_| format!("--path is 1, 2 or 3, not {v:?}"))?;
            if !(1..=3).contains(&parsed) {
                return Err(format!("--path is 1, 2 or 3, not {parsed}"));
            }
            path = Some(parsed);
        } else {
            positional.push(a);
        }
    }
    if positional.len() > 4 {
        return Err(format!("four numbers only, not {}", positional.len()));
    }
    let (Some(f), Some(b), Some(sf), Some(cr)) = (
        positional.first().and_then(|v| v.parse::<f64>().ok()),
        positional.get(1).and_then(|v| v.parse::<f64>().ok()),
        positional.get(2).and_then(|v| v.parse::<u8>().ok()),
        positional.get(3).and_then(|v| v.parse::<u8>().ok()),
    ) else {
        return Err("needs four numbers: <MHz> <kHz> <SF> <CR>".to_string());
    };
    Ok((f, b, sf, cr, path, power))
}

/// The rows `ldmux list` prints.
///
/// **Live rows by default.** The registry keeps every shell that ever ran on this machine, so
/// the list was a page of `dead` rows over the handful that are running (39 rows, 10 live, on
/// the machine this was written on); a person reading `ldmux list` saw the dead ones first.
/// `--all` is the old behaviour, and it is the only way to see why a shell ended.
fn live_rows(sessions: &[serde_json::Value], all: bool) -> Vec<&serde_json::Value> {
    sessions
        .iter()
        .filter(|s| all || s.get("status").and_then(|t| t.as_str()) != Some("dead"))
        .collect()
}

/// The first option no command knows, or None.
///
/// Every flag here is one a command actually reads. The list is closed on
/// purpose: a flag that is accepted and ignored is worse than an unknown one,
/// because the command runs and does something other than what was asked.
fn reject_unknown_flag(rest: &[String]) -> Option<String> {
    const KNOWN_FLAGS: &[&str] = &[
        "--all",
        "--cwd",
        "--history",
        "--json",
        "--mesh",
        "--mesh-only",
        "--name",
        "--power",
        "--explain",
        "--readonly",
        "--kind",
        "--note",
        "--session",
        // `ldmux mesh preset … --path N` is the one flag that takes a value of its own and
        // belongs to a subcommand rather than to the verb, so it is listed here and parsed
        // there.
        "--path",
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
/// The value of `--name value` or `--name=value`, or `None`.
///
/// `buzz` takes three optional flags with arguments, and reading them here keeps the argument
/// parser in one place instead of three ad-hoc scans.
fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if let Some(v) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
            return Some(v);
        }
        if a == name {
            return args.get(i + 1).map(String::as_str);
        }
        i += 1;
    }
    None
}

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

/// Run the attach client in this terminal and make its process the session's window.
///
/// The client is the window, so it starts first and its pid is what the daemon records. An
/// absolute binary: a bare `ldmuxd` is whatever `PATH` happens to hold. This is the one path
/// both `run`/`shell` and `attach` take, so the two cannot disagree about what "a window" is.
async fn attach_here(sock: &str, id: &str, readonly: bool) -> ! {
    let mut argv = vec!["attach".to_string(), id.to_string()];
    if readonly {
        argv.push("--readonly".to_string());
    }
    let mut child = match std::process::Command::new(ldmux_daemon::desktop::attach_bin())
        .args(&argv)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ldmux: could not start the attach client: {e}");
            std::process::exit(1);
        }
    };
    let reg: Result<serde_json::Value, CallError> = daemon_call(
        sock,
        "deskWindow",
        &serde_json::json!({ "id": id, "spawn": false, "pid": child.id() as i32 }),
    )
    .await;
    let registered = matches!(&reg, Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false));
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
    // Which build this is. No daemon needed: it is a fact about the binary, and asking it must
    // work when nothing is running (RELEASE-AUDIT 14.2).
    if cmd == "--version" || cmd == "-V" || cmd == "version" {
        println!(
            "ldmux {} (protocol {})",
            ldmux_protocol::BUILD_ID,
            ldmux_protocol::rpc::DAEMON_PROTOCOL_VERSION
        );
        std::process::exit(0);
    }
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        match command_help(&cmd) {
            Some(h) => println!("{h}"),
            None => print_usage(),
        }
        std::process::exit(0);
    }
    // `terminal --exec <command…>` hands the rest of the line to another program, which has
    // its own flags; scanning them here would reject `--exec sh -c …`.
    let exec_passthrough = cmd == "terminal" && rest.iter().any(|a| a == "--exec");
    if let Some(bad) = (!exec_passthrough).then(|| reject_unknown_flag(rest)).flatten() {
        eprintln!("ldmux {cmd}: unknown option {bad}");
        print_usage();
        std::process::exit(2);
    }

    let sock = default_socket_path();

    match cmd.as_str() {
        "ping" => {
            let r: Result<PingResult, CallError> =
                daemon_call(&sock, method::PING, &serde_json::json!({})).await;
            match r {
                Ok(r) => println!("{}", serde_json::to_string(&r).unwrap()),
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "list" => {
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, method::LIST, &serde_json::json!({})).await;
            match r {
                Ok(v) => {
                    // `--json` is a contract: it is JSON whatever the answer is, including no
                    // sessions at all. The human format is what "no sessions" is for.
                    if rest.iter().any(|a| a == "--json") {
                        println!("{}", serde_json::to_string(&v).unwrap());
                        return;
                    }
                    let sessions = v
                        .get("sessions")
                        .and_then(|s| s.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let all = rest.iter().any(|a| a == "--all");
                    let shown = live_rows(&sessions, all);
                    if shown.is_empty() {
                        if sessions.is_empty() {
                            println!("(no sessions)");
                        } else {
                            println!(
                                "(no live sessions; `ldmux list --all` shows the {} that ended)",
                                sessions.len()
                            );
                        }
                    } else {
                        for s in &shown {
                            let id = s.get("id").and_then(|t| t.as_str()).unwrap_or("");
                            let status = s.get("status").and_then(|t| t.as_str()).unwrap_or("");
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
                            println!("{id}  {status:7} {cwd}  \"{title}\"  {attach}{why}");
                        }
                    }
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "terminal" => {
            // **One resolver, asked by anything that needs a terminal.** The keybinding
            // wrapper carried its own copy of the chain in shell, so a change to the rules
            // had to be made twice and the two drifted. This answers with the program, says
            // where the answer came from, or replaces this process with `<program> -e
            // <command>` — the emulator's own template applied, which is the part a shell
            // cannot know. `set`/`clear` pin the choice in a file the daemon can read.
            if let Some(sub) = rest.first().map(String::as_str)
                && (sub == "set" || sub == "clear")
            {
                let target = if sub == "set" {
                    rest.get(1).map(String::as_str)
                } else {
                    None
                };
                if sub == "set" && target.is_none() {
                    eprintln!("usage: ldmux terminal set <program> | clear");
                    std::process::exit(2);
                }
                match ldmux_daemon::desktop::set_terminal(target) {
                    Ok(path) => {
                        match target {
                            Some(t) => println!("{t}"),
                            None => println!("cleared"),
                        }
                        eprintln!("ldmux: kept in {}", path.display());
                    }
                    Err(e) => {
                        eprintln!("ldmux: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            let explain = rest.iter().any(|a| a == "--explain");
            let (t, source) = ldmux_daemon::desktop::terminal_with_source();
            if let Some(why) = ldmux_daemon::desktop::terminal_problem(&t) {
                eprintln!("ldmux: {why}");
                std::process::exit(1);
            }
            if explain {
                println!("{t}");
                println!("  from {}", source.describe());
                return;
            }
            match rest.iter().position(|a| a == "--exec") {
                None => println!("{t}"),
                Some(at) => {
                    let command: Vec<String> = rest.iter().skip(at + 1).cloned().collect();
                    if command.is_empty() {
                        eprintln!("usage: ldmux terminal [--exec <command...>]");
                        std::process::exit(2);
                    }
                    // No class: nothing has to find this window later, and inventing one
                    // would only make an emulator set a class nobody asked for.
                    let argv = ldmux_daemon::desktop::exec_argv(&t, "", &command);
                    use std::os::unix::process::CommandExt as _;
                    let err = std::process::Command::new(&argv[0]).args(&argv[1..]).exec();
                    eprintln!("ldmux: could not run {}: {err}", argv[0]);
                    std::process::exit(1);
                }
            }
        }
        "buzz" => {
            // **The shell names itself.** Every child of a shell gets `LDMUX_SESSION_ID`
            // (`session_spawn_env`), so the common caller — an agent at the end of a turn — needs no
            // argument and cannot buzz the wrong shell by copying one. `--session` is for a process
            // outside a shell (a harness manager, a script on the host).
            let session = flag_value(rest, "--session")
                .map(str::to_owned)
                .or_else(|| std::env::var("LDMUX_SESSION_ID").ok())
                .filter(|v| !v.is_empty());
            let Some(session) = session else {
                eprintln!("ldmux buzz: run this inside an ldmux shell, or pass --session <id>");
                std::process::exit(2);
            };
            let kind = flag_value(rest, "--kind").unwrap_or("done");
            if kind != "done" && kind != "attention" {
                eprintln!("ldmux buzz: --kind is `done` or `attention`");
                std::process::exit(2);
            }
            let note = flag_value(rest, "--note").unwrap_or("");
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                method::BUZZ,
                &serde_json::json!({ "session": session, "kind": kind, "note": note }),
            )
            .await;
            // **Refused is refused.** The daemon answers `{"ok": false, "detail": "..."}` for a shell
            // that does not exist and for a repeat inside the coalescing window, and reading only
            // `Err` here made a typo'd id exit 0 in silence — the one case a script cannot see.
            // An agent still ignores the exit code: a failed buzz is not a failed turn.
            match r {
                Ok(v) if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) => {}
                Ok(v) => {
                    let detail = v
                        .get("detail")
                        .and_then(|d| d.as_str())
                        .unwrap_or("the daemon refused it");
                    eprintln!("ldmux buzz: {detail}");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("ldmux buzz: {e}");
                    std::process::exit(1);
                }
            }
        }
        "title" => {
            // **The shell names itself**, exactly as `buzz` does: an agent at the end of a turn
            // runs `ldmux title "Radio paths"` and cannot rename another shell by copying an id.
            // `--session` is for a process outside a shell.
            let session = flag_value(rest, "--session")
                .map(str::to_owned)
                .or_else(|| std::env::var("LDMUX_SESSION_ID").ok())
                .filter(|v| !v.is_empty());
            let Some(session) = session else {
                eprintln!("ldmux title: run this inside an ldmux shell, or pass --session <id>");
                std::process::exit(2);
            };
            let words: Vec<&str> = positional(rest, &["--session"])
                .into_iter()
                .filter(|w| !w.starts_with("--"))
                .collect();
            let title = words.join(" ");
            // **What only this process knows.** The agent's environment is inherited by the shell's
            // children, not by the daemon: pass the pi session file and the Claude Code marker
            // along, so the daemon renames the conversation that actually asked instead of
            // guessing from the folder.
            let hint = ldmux_daemon::title::AgentHint::from_env(&|k| std::env::var(k).ok());
            let mut agent = serde_json::Map::new();
            if let Some(file) = &hint.pi_session_file {
                agent.insert(
                    "piSessionFile".into(),
                    serde_json::Value::String(file.to_string_lossy().into_owned()),
                );
            }
            if hint.claude_code {
                agent.insert("claudeCode".into(), serde_json::Value::Bool(true));
            }
            let r: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                method::TITLE,
                &serde_json::json!({
                    "session": session,
                    "title": title,
                    "agent": serde_json::Value::Object(agent),
                }),
            )
            .await;
            // **A refusal is printed and exits non-zero.** A title is a label, not a control, so
            // nothing downstream retries it; but a typo'd id or a fourth word has to be visible.
            match r {
                Ok(v) if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) => {
                    let named = v.get("title").and_then(|t| t.as_str()).unwrap_or("");
                    match v.get("agent").and_then(|a| a.as_str()) {
                        Some("named") => println!("{named} · agent session named"),
                        Some("failed") => println!("{named} · agent session not renamed"),
                        _ => println!("{named}"),
                    }
                }
                Ok(v) => {
                    let detail = v
                        .get("detail")
                        .and_then(|d| d.as_str())
                        .unwrap_or("the daemon refused it");
                    eprintln!("ldmux title: {detail}");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("ldmux title: {e}");
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
                die(&e, &sock);
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
        "kill" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, method::KILL, &serde_json::json!({ "id": id })).await;
            if let Err(e) = r {
                die(&e, &sock);
            }
            println!("killed {id}");
        }
        "close" => {
            let pos = positional(rest, &[]);
            let id = pos.first().copied().unwrap_or("");
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, method::CLOSE, &serde_json::json!({ "id": id })).await;
            match r {
                Ok(v) => {
                    let ended = v.get("ended").and_then(|t| t.as_bool()).unwrap_or(false);
                    if ended {
                        println!("closed {id}");
                    } else {
                        // `ended: false` means a claim exists, and the holder may be the
                        // phone or the radio link as well as a desk window. Naming a desk
                        // window here was a guess printed as a fact.
                        println!("let go of {id} (another device holds it)");
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
                daemon_call(&sock, method::FORGET, &serde_json::json!({ "id": id })).await;
            match r {
                Ok(v) => {
                    let removed = v.get("removed").and_then(|t| t.as_bool()).unwrap_or(false);
                    if removed {
                        println!("forgot {id}");
                    } else {
                        // A no-op is not success: every other verb exits non-zero for an
                        // unknown id, and a script that forgets a record and gets 0 cannot
                        // tell "removed" from "was never there".
                        eprintln!("ldmux: no such session {id}");
                        std::process::exit(1);
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
            if keeper_name.is_empty() {
                eprintln!("usage: ldmux adopt <keeper-name>");
                std::process::exit(2);
            }
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
        "mesh" => {
            let sub = mesh_subcommand(rest);
            if sub == "preset" {
                // `ldmux mesh preset <MHz> <kHz> <SF> <CR>`: tune the host's OWN radio.
                let n: Vec<&str> = rest.iter().skip(1).map(String::as_str).collect();
                let parsed = preset_args(&n);
                let (f, b, sf, cr, path, power) = match parsed {
                    Ok(v) => v,
                    Err(why) => {
                        eprintln!("{why}");
                        eprintln!(
                            "usage: ldmux mesh preset <MHz> <kHz> <SF> <CR> [--path 1|2|3] [--power dBm]"
                        );
                        eprintln!("  e.g. ldmux mesh preset 910.525 62.5 7 5 --path 1");
                        std::process::exit(2);
                    }
                };
                let mut params = serde_json::json!({ "freqMhz": f, "bwKhz": b, "sf": sf, "cr": cr });
                if let Some(p) = path {
                    params["pathBytes"] = serde_json::json!(p);
                }
                if let Some(dbm) = power {
                    params["powerDbm"] = serde_json::json!(dbm);
                }
                let r: Result<serde_json::Value, CallError> =
                    daemon_call(&sock, "mesh.preset", &params).await;
                match r {
                    Ok(v) if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => {
                        // What was asked for, in the same shape `mesh status` prints, so the two
                        // lines can be compared by eye.
                        let w = path.map_or_else(String::new, |p| format!("  path {p} B"));
                        let dbm = power.map_or_else(String::new, |d| format!("  {d} dBm"));
                        println!("tuned: {f} MHz  {b} kHz  SF{sf}  CR{cr}{w}{dbm}");
                    }
                    Ok(v) => {
                        let detail = v
                            .get("detail")
                            .and_then(|d| d.as_str())
                            .unwrap_or("the radio refused the change");
                        eprintln!("ldmux mesh preset: {detail}");
                        std::process::exit(1);
                    }
                    Err(e) => die(&e, &sock),
                }
                return;
            }
            if sub != "status" {
                eprintln!("ldmux mesh: unknown subcommand '{sub}'");
                eprintln!("usage: ldmux mesh status [--json]");
                eprintln!("       ldmux mesh preset <MHz> <kHz> <SF> <CR> [--path 1|2|3]");
                std::process::exit(2);
            }
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, method::MESH_STATUS, &serde_json::json!({})).await;
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
        "clipboard-write" => {
            let pos = positional(rest, &[]);
            let arg = pos.first().copied();
            if arg.map(|a| a != "on" && a != "off").unwrap_or(false) {
                eprintln!("usage: ldmux clipboard-write on|off");
                std::process::exit(2);
            }
            let on = arg == Some("on");
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, method::CLIPBOARD_WRITE, &serde_json::json!({ "on": on })).await;
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
            let mesh_only = rest.iter().any(|a| a == "--mesh-only");
            if mesh_only && rest.iter().any(|a| a == "--mesh") {
                eprintln!("ldmux: --mesh and --mesh-only are alternatives, not both");
                std::process::exit(2);
            }
            if mesh_only {
                let r: Result<serde_json::Value, CallError> =
                    daemon_call(&sock, method::MESH_PAIRING, &serde_json::json!({})).await;
                let v = match r {
                    Ok(v) => v,
                    Err(e) => {
                        die(&e, &sock);
                    }
                };
                let Some(keys) = mesh_keys_from_json(&v) else {
                    eprintln!("ldmux: the daemon's mesh.pairing answer was not two 64-hex keys");
                    std::process::exit(1);
                };
                let name = machine_hostname();
                // `mesh.pairing` is a grant, not a read: it clears the key the host pinned for
                // whichever phone enrolled last. Printing a code therefore re-opens pairing, and
                // that phone stops being accepted the moment this runs. Said here because the
                // other half of the effect (the phone going quiet at the handshake) is
                // indistinguishable from "the host is off" at the far end.
                let pinned: Option<String> = daemon_call(&sock, method::MESH_STATUS, &serde_json::json!({}))
                    .await
                    .ok()
                    .and_then(|status: serde_json::Value| {
                        status
                            .get("session")
                            .and_then(|s| s.get("peerKey"))
                            .and_then(|k| k.as_str())
                            .map(str::to_owned)
                    });
                if let Some(key) = pinned {
                    let short: String = key.chars().take(8).collect();
                    eprintln!(
                        "ldmux: this clears the phone key pinned for {short}…; that phone must scan \
                         this code again before it can connect"
                    );
                }
                // The code can be printed either way — the two keys in it are real — but a
                // phone that scans one without a host radio cannot address anything, so say
                // it here rather than let it fail later as "host not answering".
                if v.get("hostRadio").and_then(|x| x.as_str()).is_none() {
                    eprintln!(
                        "ldmux: warning: this host's radio key is not set, so the phone cannot use \
                         this code; open the radio (LDMUX_MESH_SERIAL) so the daemon reads its key, \
                         or set LDMUX_MESH_SELF, and print it again"
                    );
                }
                if cmd == "code" {
                    match ldmux_daemon::pairing::mesh_only_code_text(&name, &keys) {
                        Ok(text) => print!("{text}"),
                        Err(problem) => {
                            eprintln!("ldmux: {}", problem.message(""));
                            std::process::exit(1);
                        }
                    }
                } else {
                    match ldmux_protocol::pairing::mesh_only_link(&name, &keys) {
                        Ok(link) => println!("{link}"),
                        Err(e) => {
                            eprintln!("ldmux: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                return;
            }
            let r: Result<serde_json::Value, CallError> =
                daemon_call(&sock, method::REMOTE_INFO, &serde_json::json!({})).await;
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
                    daemon_call(&sock, method::MESH_PAIRING, &serde_json::json!({})).await;
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
        "new" => {
            // **Create a shell and say what it is; do not open anything.** No window, no
            // attach: a caller that wants to look at it runs `ldmux attach <id>` in its own
            // terminal, and a phone picks it up from the list. This is the one creating verb
            // that is useful in a script, because it returns instead of taking the terminal.
            let pos = positional(rest, &[]);
            let cwd_default = current_dir_or_exit();
            let cwd: &str = pos.first().copied().unwrap_or(&cwd_default);
            let created: Result<serde_json::Value, CallError> = daemon_call(
                &sock,
                "new",
                &serde_json::json!({ "cwd": cwd, "createdBy": "local" }),
            )
            .await;
            match created {
                Ok(v) => {
                    let id = v
                        .get("session")
                        .and_then(|s| s.get("id"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    if id.is_empty() {
                        eprintln!("ldmux: the daemon created nothing: {v}");
                        std::process::exit(1);
                    }
                    println!("{id}");
                }
                Err(e) => {
                    die(&e, &sock);
                }
            }
        }
        "shell" => {
            // Both create a shell and become its desk window in this terminal.
            // `shell [--name N] [--cwd DIR] [dir]` reuses one already in that directory:
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
            let positional_cwd = pos.first().copied().map(|s| s.to_string());
            let cwd_default = current_dir_or_exit();
            let want_cwd: String = flag(rest, "--cwd")
                .map(|s| s.to_string())
                .or(positional_cwd)
                .unwrap_or(cwd_default);
            let name = flag(rest, "--name").map(|s| s.to_string());

            let mut reused: Option<String> = None;
            {
                let listed: Result<serde_json::Value, CallError> =
                    daemon_call(&sock, method::LIST, &serde_json::json!({})).await;
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
                            // **A shell that already has a window is not the one to reuse.** The
                            // reuse rule exists for "get me back to the shell in this directory",
                            // and a window is the thing that is already there to go back to. Reusing
                            // it here meant the new window was refused - "that shell already has a
                            // desk window" - and exited inside a second, so `$mod+Return` in the
                            // directory a person works in flashed a window open and shut. A
                            // keybinding that is supposed to always give a terminal gives one.
                            let windowed = s
                                .get("liveWorkspace")
                                .map(|v| !v.is_null())
                                .unwrap_or(false);
                            if !(is_shell && alive && backed) || windowed {
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

            attach_here(&sock, &id, false).await;
        }
        "attach" => {
            // **The desk client for a terminal you already have.** The daemon opens a
            // window only for the phone (`send to desktop`); a person at a prompt is already
            // in the terminal they want, so this runs the attach client in it, with no
            // emulator and no spawn. The selector is a session id or a unique
            // prefix, the same shape every other verb takes.
            let readonly = rest.iter().any(|a| a == "--readonly");
            let pos: Vec<&str> = rest
                .iter()
                .map(std::string::String::as_str)
                .filter(|a| !a.starts_with("--"))
                .collect();
            let Some(sel) = pos.first().copied().filter(|s| !s.is_empty()) else {
                eprintln!("usage: ldmux attach <id|prefix> [--readonly]");
                std::process::exit(2);
            };
            let listed: Result<serde_json::Value, CallError> =
                daemon_call(&sock, method::LIST, &serde_json::json!({})).await;
            let v = match listed {
                Ok(v) => v,
                Err(e) => {
                    die(&e, &sock);
                }
            };
            let empty = Vec::new();
            let rows = v
                .get("sessions")
                .and_then(|s| s.as_array())
                .unwrap_or(&empty);
            let alive = |s: &serde_json::Value| {
                s.get("status").and_then(|t| t.as_str()) != Some("dead")
                    && s.get("keeperName")
                        .and_then(|t| t.as_str())
                        .map(|k| !k.is_empty())
                        .unwrap_or(false)
            };
            let ids: Vec<&str> = rows
                .iter()
                .filter(|s| alive(s))
                .filter_map(|s| s.get("id").and_then(|t| t.as_str()))
                .collect();
            let id = match ids.iter().find(|i| **i == sel) {
                Some(exact) => (*exact).to_string(),
                None => {
                    let matches: Vec<&&str> = ids.iter().filter(|i| i.starts_with(sel)).collect();
                    match matches.as_slice() {
                        [one] => (**one).to_string(),
                        [] => {
                            eprintln!("ldmux: no live session matches {sel:?}");
                            std::process::exit(1);
                        }
                        _ => {
                            let mut names: Vec<&str> = matches.iter().map(|m| **m).collect();
                            names.sort_unstable();
                            eprintln!(
                                "ldmux: {sel:?} matches {} sessions: {}",
                                names.len(),
                                names.join(", ")
                            );
                            std::process::exit(2);
                        }
                    }
                }
            };
            attach_here(&sock, &id, readonly).await;
        }
        _ => {
            eprintln!("ldmux: unknown command {cmd}");
            print_usage();
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::{live_rows, mesh_subcommand};
    use serde_json::json;

    /// `ldmux mesh --json` is the documented form and must mean `status`.
    #[test]
    fn a_flag_selects_the_default_mesh_subcommand() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(mesh_subcommand(&args(&[])), "status");
        assert_eq!(mesh_subcommand(&args(&["--json"])), "status");
        assert_eq!(mesh_subcommand(&args(&["status", "--json"])), "status");
        assert_eq!(mesh_subcommand(&args(&["preset", "910.525"])), "preset");
    }

    /// Live rows are what `ldmux list` shows; `--all` restores every record.
    #[test]
    fn list_shows_live_rows_unless_all_is_asked_for() {
        let sessions = vec![
            json!({"id": "a", "status": "running"}),
            json!({"id": "b", "status": "dead"}),
            json!({"id": "c", "status": "running"}),
        ];
        let ids = |v: Vec<&serde_json::Value>| {
            v.iter()
                .map(|s| s["id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(live_rows(&sessions, false)), vec!["a", "c"]);
        assert_eq!(ids(live_rows(&sessions, true)), vec!["a", "b", "c"]);
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

#[cfg(test)]
mod preset_args_tests {
    use super::preset_args;

    #[test]
    fn the_four_numbers_alone_leave_the_width_alone() {
        assert_eq!(
            preset_args(&["910.525", "62.5", "7", "5"]),
            Ok((910.525, 62.5, 7, 5, None, None))
        );
    }

    #[test]
    fn a_width_is_bytes_and_may_come_before_the_numbers() {
        assert_eq!(
            preset_args(&["910.525", "62.5", "7", "5", "--path", "1"]),
            Ok((910.525, 62.5, 7, 5, Some(1), None))
        );
        assert_eq!(
            preset_args(&["--path", "2", "910.525", "500", "7", "5"]),
            Ok((910.525, 500.0, 7, 5, Some(2), None))
        );
    }

    #[test]
    fn power_is_dbm_and_rides_with_the_tuple() {
        assert_eq!(
            preset_args(&["910.525", "62.5", "7", "5", "--path", "1", "--power", "14"]),
            Ok((910.525, 62.5, 7, 5, Some(1), Some(14)))
        );
        // A negative power is legal: the firmware's window starts at -9 dBm.
        assert_eq!(
            preset_args(&["--power", "-9", "910.525", "62.5", "7", "5"]),
            Ok((910.525, 62.5, 7, 5, None, Some(-9)))
        );
        assert_eq!(
            preset_args(&["910.525", "62.5", "7", "5", "--power", "31"]).unwrap_err(),
            "--power is -9 to 30 dBm, not 31"
        );
        assert_eq!(
            preset_args(&["910.525", "62.5", "7", "5", "--power"]).unwrap_err(),
            "--power needs a number of dBm: -9 to 30"
        );
    }

    #[test]
    fn a_width_the_firmware_cannot_hold_is_refused_with_a_sentence() {
        assert_eq!(
            preset_args(&["910.525", "62.5", "7", "5", "--path", "4"]).unwrap_err(),
            "--path is 1, 2 or 3, not 4"
        );
        assert_eq!(
            preset_args(&["910.525", "62.5", "7", "5", "--path"]).unwrap_err(),
            "--path needs a number: 1, 2 or 3"
        );
    }

    #[test]
    fn a_missing_or_extra_number_is_named() {
        assert_eq!(
            preset_args(&["910.525", "62.5", "7"]).unwrap_err(),
            "needs four numbers: <MHz> <kHz> <SF> <CR>"
        );
        assert_eq!(
            preset_args(&["910.525", "62.5", "7", "5", "9"]).unwrap_err(),
            "four numbers only, not 5"
        );
    }
}
