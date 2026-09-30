//! The ldmux attach client: the desktop window client (plan §3.9).
//!
//! In production it is `alacritty -e ldmuxd attach <id>` — its stdio **is**
//! alacritty's pty (it opens no pty pair of its own), and it connects to the
//! keeper/daemon and pipes the session's bytes. Here it is a **skeleton**
//! (the gate is "attach boots headless"): it takes two fds for its local
//! terminal (where it reads user input, and where it writes session output),
//! attaches to a keeper session, and pipes bytes in both directions.
//!
//! New (no TS source of record): the desktop shim dies; the attach client is
//! new. Design of record: `docs/RUST-PORT-PLAN.md` §3.9.

use ldmux_protocol::keeper::{self as km, KeeperMessage};
use ldmux_protocol::keeper_call::{keeper_call, KeeperCallError, DEFAULT_TIMEOUT_MS};

use std::os::raw::c_int;

/// An attach client: pipes a session's bytes ↔ a local terminal.
///
/// `in_fd` is where user input is read (the local terminal), `out_fd` is where
/// session output is written. In the headless skeleton they are the two ends
/// of a pty pair's slave (the test drives the master); in production they are
/// the process's stdio (alacritty's pty).
pub struct Attach {
    socket: String,
    session_id: String,
    in_fd: c_int,
    out_fd: c_int,
}

impl Attach {
    /// Connect to the keeper and attach to `session_id` (the Read handshake).
    ///
    /// Returns an error if the session is dead or the keeper rejects the
    /// attach.
    pub async fn connect(
        socket: &str,
        session_id: &str,
        in_fd: c_int,
        out_fd: c_int,
    ) -> Result<Attach, String> {
        let req = KeeperMessage::Read(km::Read {
            session_id: session_id.to_string(),
        });
        let res: KeeperMessage = keeper_call(socket, &req, DEFAULT_TIMEOUT_MS)
            .await
            .map_err(|e| e.0)?;
        match res {
            KeeperMessage::State(st) => {
                if st.dead {
                    return Err(format!("session {session_id} is dead"));
                }
            }
            other => return Err(format!("expected State, got {other:?}")),
        }
        Ok(Attach {
            socket: socket.to_string(),
            session_id: session_id.to_string(),
            in_fd,
            out_fd,
        })
    }

    /// Read one chunk of user input from the local terminal (non-blocking:
    /// polls `in_fd`, then reads whatever is ready). Returns an empty `Vec`
    /// when nothing is ready (or at EOF). The caller polls in a loop.
    pub fn read_input(&self) -> Vec<u8> {
        let mut pfd = nix::libc::pollfd {
            fd: self.in_fd,
            events: nix::libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { nix::libc::poll(&mut pfd, 1, 0) };
        if n <= 0 {
            return Vec::new();
        }
        let mut buf = [0u8; 4096];
        let n = unsafe { nix::libc::read(self.in_fd, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n <= 0 {
            Vec::new()
        } else {
            buf[..n as usize].to_vec()
        }
    }

    /// Send input to the session (the Write direction).
    pub async fn send_input(&self, data: &str) -> Result<(), String> {
        let req = KeeperMessage::Write(km::Write {
            session_id: self.session_id.clone(),
            data: data.to_string(),
        });
        let res: KeeperMessage =
            keeper_call(&self.socket, &req, DEFAULT_TIMEOUT_MS).await.map_err(|e| e.0)?;
        match res {
            KeeperMessage::Ack(_) => Ok(()),
            other => Err(format!("expected Ack, got {other:?}")),
        }
    }

    /// Pull the session's output bytes (the Peek direction): drains the
    /// keeper's buffered output for this session.
    pub async fn read_output(&self) -> Result<Vec<u8>, String> {
        let req = KeeperMessage::Peek(km::Peek {
            session_id: self.session_id.clone(),
        });
        let res: KeeperMessage =
            keeper_call(&self.socket, &req, DEFAULT_TIMEOUT_MS).await.map_err(|e| e.0)?;
        match res {
            KeeperMessage::PeekResult(pr) => Ok(pr.data),
            other => Err(format!("expected PeekResult, got {other:?}")),
        }
    }

    /// Write session output to the local terminal (blocking write on
    /// `out_fd`).
    pub fn write_output(&self, data: &[u8]) {
        let mut off = 0usize;
        while off < data.len() {
            let n = unsafe {
                nix::libc::write(
                    self.out_fd,
                    data[off..].as_ptr() as *const _,
                    data.len() - off,
                )
            };
            if n <= 0 {
                break;
            }
            off += n as usize;
        }
    }

    /// One round of the protocol loop: pull the session's output and push it
    /// to the local terminal. Returns the bytes pushed (the test inspects
    /// them); a full client runs this in a loop with input handling.
    pub async fn pump_output(&self) -> Result<Vec<u8>, String> {
        let out = self.read_output().await?;
        if !out.is_empty() {
            self.write_output(&out);
        }
        Ok(out)
    }

    /// The session id (for the test / diagnostics).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// A failure answer from the keeper, as an error.
    #[allow(dead_code)]
    fn keeper_error(e: &KeeperCallError) -> String {
        e.0.clone()
    }
}
