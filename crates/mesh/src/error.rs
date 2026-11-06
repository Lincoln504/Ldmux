//! The error every layer of the radio route shares.
//!
//! This module held the L2 frame codec while the loopback gateway existed. The gateway is gone,
//! and with it the frames: the host's carriage is the companion protocol, which owns its own
//! framing (`crates/companion/src/framing.rs`). What remains is the error type the carrier seam,
//! the serial carrier and the session loop all report with, kept at this module path because
//! every one of them imports it from here.

use std::fmt;

/// Everything that can go wrong below the terminal protocol.
#[derive(Debug)]
pub enum MeshError {
    /// The carriage closed.
    Closed,
    /// An exchange did not finish in time.
    ///
    /// Distinct from [`Self::Closed`]: a radio that accepted the port and then went quiet is a
    /// different failure from one that hung up, and the sentence an operator reads says which.
    Timeout(&'static str),
    /// The port, socket or device failed.
    Io(std::io::Error),
    /// The carriage cannot do what was asked of it: a radio that refused a tuple, a carriage
    /// that has no radio to tune.
    Unsupported(String),
}

impl fmt::Display for MeshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "the link closed"),
            Self::Timeout(what) => write!(f, "{what} timed out"),
            Self::Io(e) => write!(f, "the link: {e}"),
            Self::Unsupported(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for MeshError {}

impl From<std::io::Error> for MeshError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
