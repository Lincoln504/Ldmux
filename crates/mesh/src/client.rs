//! A loopback client for the MeshMore gateway.
//!
//! One connection, one session, no retries and no reconnection: those belong to
//! the layer that owns the session, because only it knows whether a record was
//! already handed to the radio. This client's whole job is to turn method calls
//! into frames and frames back into values.
//!
//! A background task owns the read half, so a caller may send while waiting and
//! neither direction can deadlock the other.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::constants::{
    BindError, CloseReason, FrameType, HelloError, PairError, RadioState, SendOutcome,
};
use crate::frame::{Decoder, Frame, MeshError};
use crate::messages::{self, BindOk, HelloOk, SendResult};

/// What came out of the read task.
enum Event {
    Frame(Frame),
    Failed(MeshError),
    Closed,
}

/// The result of a complete handshake.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Handshake {
    pub hello: HelloOk,
    pub bind: BindOk,
}

/// A connected gateway session.
pub struct MeshClient {
    writer: OwnedWriteHalf,
    events: mpsc::Receiver<Event>,
    reader: JoinHandle<()>,
}

impl MeshClient {
    /// Connects and starts the read task. No frame is sent yet: a caller that
    /// never handshakes wastes no radio time.
    pub async fn connect(addr: impl ToSocketAddrs) -> Result<Self, MeshError> {
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        let (mut read, write) = stream.into_split();
        let (tx, rx) = mpsc::channel(64);
        let reader = tokio::spawn(async move {
            let mut decoder = Decoder::new();
            let mut buf = [0u8; 1024];
            loop {
                match read.read(&mut buf).await {
                    Ok(0) => {
                        let _ = tx.send(Event::Closed).await;
                        return;
                    }
                    Ok(n) => match decoder.push(&buf[..n]) {
                        Ok(frames) => {
                            for frame in frames {
                                if tx.send(Event::Frame(frame)).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Event::Failed(e)).await;
                            return;
                        }
                    },
                    Err(e) => {
                        let _ = tx.send(Event::Failed(MeshError::Io(e))).await;
                        return;
                    }
                }
            }
        });
        Ok(Self {
            writer: write,
            events: rx,
            reader,
        })
    }

    /// Writes one frame.
    pub async fn send(&mut self, frame: &Frame) -> Result<(), MeshError> {
        self.writer.write_all(&frame.encode()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// The next frame from the gateway. Wrap it in `tokio::time::timeout` for a
    /// deadline; this method waits indefinitely.
    pub async fn recv(&mut self) -> Result<Frame, MeshError> {
        match self.events.recv().await {
            Some(Event::Frame(frame)) => Ok(frame),
            Some(Event::Failed(e)) => Err(e),
            Some(Event::Closed) | None => Err(MeshError::Closed),
        }
    }

    /// `HELLO`, then `PAIR`.
    pub async fn hello_and_pair(
        &mut self,
        token: &[u8],
        capabilities: u8,
    ) -> Result<HelloOk, MeshError> {
        self.send(&messages::hello(capabilities)).await?;
        let hello = loop {
            let frame = self.recv().await?;
            match frame.kind {
                FrameType::HelloOk => {
                    let hello = messages::read_hello_ok(&frame)?;
                    if hello.version != crate::VERSION {
                        return Err(MeshError::Refused("hello", hello.version));
                    }
                    break hello;
                }
                FrameType::HelloErr => match messages::read_hello_err(&frame)? {
                    HelloError::VersionMismatch => {
                        return Err(MeshError::Refused("hello", 1));
                    }
                    HelloError::Busy => return Err(MeshError::Refused("hello", 2)),
                    HelloError::Unknown(code) => {
                        return Err(MeshError::Refused("hello", code));
                    }
                },
                FrameType::Close => {
                    return Err(MeshError::Refused(
                        "close",
                        messages::read_close(&frame)?.code(),
                    ));
                }
                // A gateway may push STATUS at any time after HELLO.
                _ => {}
            }
        };

        self.send(&messages::pair(token)?).await?;
        loop {
            let frame = self.recv().await?;
            match frame.kind {
                FrameType::PairOk => break,
                FrameType::PairErr => {
                    let code = match messages::read_pair_err(&frame)? {
                        PairError::BadToken => 1,
                        PairError::AlreadyPaired => 2,
                        PairError::Revoked => 3,
                        PairError::Unknown(code) => code,
                    };
                    return Err(MeshError::Refused("pair", code));
                }
                FrameType::Close => {
                    return Err(MeshError::Refused(
                        "close",
                        messages::read_close(&frame)?.code(),
                    ));
                }
                _ => {}
            }
        }
        Ok(hello)
    }

    /// `BIND` to a mesh peer.
    pub async fn bind(&mut self, peer_pubkey: &[u8]) -> Result<BindOk, MeshError> {
        self.send(&messages::bind(peer_pubkey)?).await?;
        loop {
            let frame = self.recv().await?;
            match frame.kind {
                FrameType::BindOk => return messages::read_bind_ok(&frame),
                FrameType::BindErr => {
                    let code = match messages::read_bind_err(&frame)? {
                        BindError::NotAContact => 1,
                        BindError::NoPath => 2,
                        BindError::Unknown(code) => code,
                    };
                    return Err(MeshError::Refused("bind", code));
                }
                FrameType::Close => {
                    return Err(MeshError::Refused(
                        "close",
                        messages::read_close(&frame)?.code(),
                    ));
                }
                _ => {}
            }
        }
    }

    /// The whole handshake: `HELLO`, `PAIR`, `BIND`.
    pub async fn handshake(
        &mut self,
        token: &[u8],
        peer_pubkey: &[u8],
        capabilities: u8,
    ) -> Result<Handshake, MeshError> {
        let hello = self.hello_and_pair(token, capabilities).await?;
        let bind = self.bind(peer_pubkey).await?;
        Ok(Handshake { hello, bind })
    }

    /// Hands one record to the gateway. The answer is a `SEND_RESULT` carrying
    /// the same `seq`; a caller that wants it should read frames until it
    /// arrives, because downlink `RECV` and `STATUS` are interleaved with it.
    pub async fn send_record(&mut self, seq: u8, record: &[u8]) -> Result<(), MeshError> {
        self.send(&messages::record(seq, record)?).await
    }

    /// Closes politely: `CLOSE(shutdown)` then a socket shutdown. The gateway
    /// treats an abrupt drop the same way, so this is courtesy, not protocol.
    pub async fn close(mut self) {
        let _ = self.send(&messages::close(CloseReason::Shutdown)).await;
        let _ = self.writer.shutdown().await;
        self.reader.abort();
    }
}

impl Drop for MeshClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// A helper for the common "wait for the frame that answers me" case.
///
/// Returns the first frame matching `want`, passing over interleaved `STATUS`
/// and `RECV` frames, which it hands to `other` so nothing is silently lost.
pub async fn recv_matching(
    client: &mut MeshClient,
    want: FrameType,
    mut other: impl FnMut(Frame),
) -> Result<Frame, MeshError> {
    loop {
        let frame = client.recv().await?;
        if frame.kind == want {
            return Ok(frame);
        }
        match frame.kind {
            FrameType::Status | FrameType::Recv | FrameType::Close => other(frame),
            _ => {}
        }
    }
}

/// Interprets a `SEND_RESULT` frame.
pub fn send_result_of(frame: &Frame) -> Result<SendResult, MeshError> {
    messages::read_send_result(frame)
}

/// Interprets a `STATUS` frame.
pub fn status_of(frame: &Frame) -> Result<messages::Status, MeshError> {
    messages::read_status(frame)
}

/// Interprets a `RECV` frame.
pub fn recv_of(frame: &Frame) -> Result<messages::RecvRecord, MeshError> {
    messages::read_recv(frame)
}

/// The radio state carried by `HELLO_OK`, for callers that only want that.
#[must_use]
pub fn radio_state_of(hello: &HelloOk) -> RadioState {
    hello.radio_state
}

/// The outcome of a record, for callers that only want that.
#[must_use]
pub fn outcome_of(result: &SendResult) -> SendOutcome {
    result.outcome
}

/// Read half is kept alive by the task; this exists so the type is nameable in
/// tests that split a stream by hand.
pub type ReaderHalf = OwnedReadHalf;
