//! The byte link under the protocol, and the fake used to test it.
//!
//! Everything above this module sees only [`ByteLink`]. That is what keeps the
//! protocol testable without a radio: [`MemoryLink`] plays a scripted firmware,
//! so the framing, the identity handshake and the raw send are all exercised in
//! `cargo test`, which is the only place they are exercised at all before a
//! hardware pass.

use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

/// The longest one command may take to reach the port before the device is reported dead.
///
/// A CDC endpoint that stays open and stops draining fills the tty's buffer; the write then
/// waits for room that will never come. That wait used to be unbounded, and the radio thread
/// parked inside it: the session loop's teardown joins that thread, so the supervisor hung
/// too, the snapshot stayed `ready / established` for ever and only a daemon restart recovered.
/// Two seconds is far longer than a 172-byte record takes to drain at 115200 baud (about
/// 15 ms) and short enough that a wedged radio is a named error, not a stuck shell.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Push `bytes` into `write` until every byte is accepted or `timeout` passes.
///
/// A serial port cannot be made to stall on demand, so the retry loop is not reachable from a
/// test through [`SerialLink`]. It lives here, generic over the syscall and the wait, so a
/// writer that never accepts a byte can prove the bound without a device.
fn write_all_within(
    bytes: &[u8],
    timeout: Duration,
    port: &str,
    mut write: impl FnMut(&[u8]) -> Result<usize, nix::errno::Errno>,
    mut wait_writable: impl FnMut(),
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut written = 0;
    while written < bytes.len() {
        match write(&bytes[written..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    format!("{port}: the port accepted no bytes"),
                ));
            }
            Ok(n) => written += n,
            Err(nix::errno::Errno::EAGAIN) => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("{port}: the radio stopped accepting bytes for {timeout:?}"),
                    ));
                }
                wait_writable();
            }
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(io::Error::from(e)),
        }
    }
    Ok(())
}

/// A bidirectional byte pipe to a companion radio.
pub trait ByteLink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Read up to `buf.len()` bytes. Returning 0 means "nothing right now"; the
    /// caller owns the timeout, because only it knows what the last command was.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

/// A real serial port, opened raw and non-blocking.
pub struct SerialLink {
    fd: OwnedFd,
    /// Where the port was opened from, so a stalled write can name it.
    path: String,
}

impl SerialLink {
    /// Open `path` at 115200 8N1, raw, with **no modem-line transition on close**.
    ///
    /// The line state matters more than it looks. An ESP32-S3 treats a DTR/RTS transition as a
    /// reset request, so a client that toggles them reboots the radio and then races its boot.
    /// This opener deliberately never toggles them: it does not raise or lower DTR/RTS on open
    /// (an `ioctl` that cleared them would itself be the transition), and it clears `HUPCL` so
    /// closing the port does not drop the lines — which is the part that turns "the daemon
    /// restarted" into "the radio rebooted and the first command raced its banner".
    pub fn open(path: &str) -> io::Result<Self> {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;

        // **O_CLOEXEC is not tidiness here, it is the radio's ownership.** The flock that makes
        // this process the one owner is held on this descriptor, and an inherited descriptor keeps
        // the lock after the owner is gone: every shell and every desk window spawned from the
        // daemon carried `/dev/ttyACM0` across `exec`, so a restarted daemon found the radio
        // "held by another process" until all of those exited (measured 2026-11-05: an `alacritty`
        // and an `ldmuxd attach` client were the lock holders, and `ldmux mesh status` reported
        // `another process holds this radio`, leaving the mesh down with the phone retrying).
        let raw = open(
            path,
            OFlag::O_RDWR | OFlag::O_NOCTTY | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        // SAFETY: `open` just returned this descriptor and nothing else owns it.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };

        let mut attrs = nix::sys::termios::tcgetattr(&fd).map_err(io::Error::from)?;
        nix::sys::termios::cfmakeraw(&mut attrs);
        attrs.control_flags |= nix::sys::termios::ControlFlags::CLOCAL | nix::sys::termios::ControlFlags::CREAD;
        nix::sys::termios::cfsetispeed(&mut attrs, nix::sys::termios::BaudRate::B115200)
            .map_err(io::Error::from)?;
        nix::sys::termios::cfsetospeed(&mut attrs, nix::sys::termios::BaudRate::B115200)
            .map_err(io::Error::from)?;
        nix::sys::termios::tcsetattr(&fd, nix::sys::termios::SetArg::TCSANOW, &attrs)
            .map_err(io::Error::from)?;

        // **Do not drop the lines on close.** See the note on `open`: the transition is the
        // reset, so the only safe line-state change is the one that does not happen.
        let mut bits = nix::sys::termios::tcgetattr(&fd).map_err(io::Error::from)?;
        bits.control_flags.remove(nix::sys::termios::ControlFlags::HUPCL);
        nix::sys::termios::tcsetattr(&fd, nix::sys::termios::SetArg::TCSANOW, &bits)
            .map_err(io::Error::from)?;

        Ok(Self {
            fd,
            path: path.to_owned(),
        })
    }

    /// One owner per radio. The tty permits several opens, so without an advisory
    /// lock two clients interleave frames on one port and each sees a stream that
    /// belongs to both.
    // `nix::fcntl::flock` is deprecated in favour of the `Flock` guard type, which
    // unlocks on drop. That is the right shape for a lock the caller holds for the
    // connection's lifetime, but the guard must then live in this struct beside the
    // fd, and the crate does not need that yet: nothing in the daemon takes the
    // lock through this type today. Keeping the raw call with an explicit allow is
    // honest about that; swapping in the guard is a two-line change when it is used.
    #[allow(deprecated)]
    pub fn lock_exclusive(&self) -> io::Result<()> {
        nix::fcntl::flock(self.fd.as_raw_fd(), nix::fcntl::FlockArg::LockExclusiveNonblock)
            .map_err(io::Error::from)
    }
}

impl ByteLink for SerialLink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let fd = &self.fd;
        let wait_writable = || {
            // Non-blocking write with no room: wait for the port to drain. The poll's own
            // timeout only bounds how often the deadline above is re-checked.
            let mut poll = nix::poll::PollFd::new(fd.as_fd(), nix::poll::PollFlags::POLLOUT);
            let _ = nix::poll::poll(
                std::slice::from_mut(&mut poll),
                nix::poll::PollTimeout::from(100u16),
            );
        };
        write_all_within(
            bytes,
            WRITE_TIMEOUT,
            &self.path,
            |slice| nix::unistd::write(fd, slice),
            wait_writable,
        )
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut poll = nix::poll::PollFd::new(self.fd.as_fd(), nix::poll::PollFlags::POLLIN);
        let ready = nix::poll::poll(std::slice::from_mut(&mut poll), nix::poll::PollTimeout::from(20u16))
            .map_err(io::Error::from)?;
        if ready == 0 {
            return Ok(0);
        }
        match nix::unistd::read(self.fd.as_raw_fd(), buf) {
            Ok(n) => Ok(n),
            Err(nix::errno::Errno::EAGAIN) => Ok(0),
            Err(e) => Err(io::Error::from(e)),
        }
    }
}

/// A scripted firmware: bytes the caller queues are what the client reads, and
/// whatever the client writes is recorded.
#[derive(Default)]
pub struct MemoryLink {
    /// Bytes waiting to be read by the client.
    pub inbound: VecDeque<u8>,
    /// Everything the client has written, in order.
    pub written: Vec<u8>,
}

impl MemoryLink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a raw device→host frame (already framed).
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.inbound.extend(bytes.iter().copied());
    }

    /// Queue one payload, framed as the radio frames it.
    pub fn push_payload(&mut self, payload: &[u8]) {
        let mut framed = Vec::with_capacity(payload.len() + 3);
        framed.push(0x3E);
        framed.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        framed.extend_from_slice(payload);
        self.push_bytes(&framed);
    }
}

impl ByteLink for MemoryLink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.written.extend_from_slice(bytes);
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.inbound.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.inbound.pop_front().expect("length was just checked");
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_link_round_trips_bytes() {
        let mut link = MemoryLink::new();
        link.push_payload(b"hello");
        assert_eq!(&link.inbound.iter().copied().collect::<Vec<_>>(), b">\x05\x00hello");
        link.write_all(b"abc").unwrap();
        assert_eq!(link.written, b"abc");
        let mut buf = [0u8; 8];
        assert_eq!(link.read(&mut buf).unwrap(), 8);
    }

    #[test]
    fn a_port_that_stops_accepting_bytes_is_reported_not_retried_for_ever() {
        let mut attempts = 0;
        let started = Instant::now();
        let e = write_all_within(
            b"a record",
            Duration::from_millis(30),
            "/dev/ttyTEST",
            |_| {
                attempts += 1;
                Err(nix::errno::Errno::EAGAIN)
            },
            || std::thread::sleep(Duration::from_millis(2)),
        )
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(e.to_string().contains("/dev/ttyTEST"), "{e}");
        assert!(attempts > 1, "it retried before giving up");
        // The bound, not the scheduler: a 30 ms timeout must not turn into seconds.
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_partial_write_is_finished_and_a_zero_is_an_error() {
        let mut chunks = vec![2usize, 3, 4].into_iter();
        write_all_within(
            b"abcdefghi",
            Duration::from_secs(1),
            "/dev/ttyTEST",
            |slice| Ok(chunks.next().unwrap_or(0).min(slice.len())),
            || {},
        )
        .unwrap();

        let e = write_all_within(
            b"x",
            Duration::from_secs(1),
            "/dev/ttyTEST",
            |_| Ok(0),
            || {},
        )
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::WriteZero);
    }

    #[test]
    fn an_empty_memory_link_reads_zero_rather_than_blocking() {
        let mut link = MemoryLink::new();
        let mut buf = [0u8; 4];
        assert_eq!(link.read(&mut buf).unwrap(), 0);
    }
}
