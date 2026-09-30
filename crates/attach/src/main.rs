//! The `ldmux-attach` binary: attach to a keeper session and pipe its bytes to
//! this process's stdio.
//!
//! In production this is `alacritty -e ldmuxd attach <id>` — its stdio is
//! alacritty's pty (fds 0 and 1), and it pipes the session's bytes to/from
//! them. The socket is `$LDMUX_KEEPER_SOCKET` or the first arg; the session id
//! is the last arg.

use std::os::raw::c_int;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    // ldmux-attach [socket] <session-id>
    let (socket, session) = match args.len() {
        2 => (
            std::env::var("LDMUX_KEEPER_SOCKET").unwrap_or_default(),
            args[1].clone(),
        ),
        3 => (args[1].clone(), args[2].clone()),
        _ => {
            eprintln!("usage: ldmux-attach [socket] <session-id>");
            std::process::exit(2);
        }
    };
    let in_fd: c_int = 0; // stdin (alacritty's pty)
    let out_fd: c_int = 1; // stdout
    match attach::Attach::connect(&socket, &session, in_fd, out_fd).await {
        Ok(a) => {
            // The loop: pull the session's output to the terminal. (Input —
            // reading stdin and sending it to the session — is wired in the
            // phase that makes the window interactive.)
            let mut buf = [0u8; 4096];
            loop {
                // Pull output (non-blocking-ish: one round per loop).
                match a.pump_output().await {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("ldmux-attach: {e}");
                        break;
                    }
                }
                // Read input from stdin; break at EOF (window closed).
                let n = unsafe { nix::libc::read(in_fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                if n <= 0 {
                    break;
                }
                let _ = a.send_input(&String::from_utf8_lossy(&buf[..n as usize])).await;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        Err(e) => {
            eprintln!("ldmux-attach: {e}");
            std::process::exit(1);
        }
    }
}
