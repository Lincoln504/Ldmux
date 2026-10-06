//! The `ldmux-attach` binary: attach this terminal to a session and pipe it.
//!
//! In production it is run as `alacritty -e ldmuxd attach <id>` (the daemon's
//! `attach` subcommand calls `attach::run`); this binary is the same entry
//! point for direct use.
//!
//! Usage: `ldmux-attach <session-id> [--readonly]`

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let mut session: Option<String> = None;
    let mut readonly = false;
    for a in args.by_ref() {
        match a.as_str() {
            "--readonly" | "-r" => readonly = true,
            other if session.is_none() => session = Some(other.to_string()),
            other => {
                eprintln!("ldmux-attach: unexpected argument {other:?}");
                std::process::exit(2);
            }
        }
    }
    let Some(session) = session else {
        eprintln!("usage: ldmux-attach <session-id> [--readonly]");
        std::process::exit(2);
    };
    if let Err(e) = attach::run(&session, readonly).await {
        eprintln!("ldmux-attach: {e}");
        std::process::exit(1);
    }
    // Exit rather than return: a read of stdin is still pending on a blocking
    // thread, and the runtime would wait for it.
    std::process::exit(0);
}