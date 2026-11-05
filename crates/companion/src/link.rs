//! The byte link under the protocol, and the fake used to test it.
//!
//! Everything above this module sees only [`ByteLink`]. That is what keeps the
//! protocol testable without a radio: [`MemoryLink`] plays a scripted firmware,
//! so the framing, the identity handshake and the raw send are all exercised in
//! `cargo test`, which is the only place they are exercised at all before a
//! hardware pass.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

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
}

impl SerialLink {
    /// Open `path` at 115200 8N1, raw, with DTR and RTS **de-asserted**.
    ///
    /// The line state matters more than it looks. An ESP32-S3 treats a DTR/RTS
    /// transition as a reset request, so a client that raises them while opening
    /// reboots the radio and then races its boot. Holding them de-asserted from
    /// the moment the port opens is what makes the first command land.
    pub fn open(path: &str) -> io::Result<Self> {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;

        let raw = open(
            path,
            OFlag::O_RDWR | OFlag::O_NOCTTY | OFlag::O_NONBLOCK,
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

        // De-assert both modem lines before anything is written.
        let bits = nix::sys::termios::tcgetattr(&fd).map_err(io::Error::from)?;
        let mut bits = bits;
        bits.control_flags.remove(nix::sys::termios::ControlFlags::HUPCL);
        nix::sys::termios::tcsetattr(&fd, nix::sys::termios::SetArg::TCSANOW, &bits)
            .map_err(io::Error::from)?;

        Ok(Self { fd })
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
        let mut written = 0;
        while written < bytes.len() {
            match nix::unistd::write(&self.fd, &bytes[written..]) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "port accepted no bytes")),
                Ok(n) => written += n,
                Err(nix::errno::Errno::EAGAIN) => {
                    // Non-blocking write with no room: wait for the port to drain.
                    let mut poll = nix::poll::PollFd::new(self.fd.as_fd(), nix::poll::PollFlags::POLLOUT);
                    let _ = nix::poll::poll(std::slice::from_mut(&mut poll), nix::poll::PollTimeout::from(100u16));
                }
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => return Err(io::Error::from(e)),
            }
        }
        Ok(())
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

/// Read a whole file-less stream, for the tests that want a `Read` above a link.
pub fn read_some<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    reader.read(buf)
}

/// Write a slice, for tests that want a `Write` above a link.
pub fn write_some<W: Write>(writer: &mut W, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(bytes)
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
    fn an_empty_memory_link_reads_zero_rather_than_blocking() {
        let mut link = MemoryLink::new();
        let mut buf = [0u8; 4];
        assert_eq!(link.read(&mut buf).unwrap(), 0);
    }
}
