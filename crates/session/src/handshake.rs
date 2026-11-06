//! The handshake (ADR-0020 section 2): `Noise_IKpsk2_25519_ChaChaPoly_SHA256` with a PSK
//! filter tag in front.
//!
//! ```text
//! message 1 (initiator, 121 B)  kind 0x11 | filter tag 16 | noise: e 32, enc s 48, enc ts 8+16
//! message 2 (responder,  50 B)  kind 0x12 | noise: e 32, enc index 1+16
//! ```
//!
//! The filter tag is HMAC-SHA-256(key = HMAC(psk, label), kind || noise message 1) cut to 16
//! bytes. It is checked before any Diffie-Hellman, so an unauthenticated flood costs one MAC.
//! It is our addition on top of Noise (the idea is WireGuard's mac1) and wants review; it
//! introduces no primitive. The PSK is never used directly outside Noise.
//!
//! Persistence is the caller's: the last accepted timestamp and the pinned phone key per
//! pairing are returned in [`Accepted`] and supplied in [`PairingEntry`].
//!
//! # The cryptography, audited 2026-11-06
//!
//! Checked against the Noise specification's own rules rather than against intuition:
//!
//! - **Two cipherstates, never one.** [`snow::StatelessTransportState`] holds the `send` and
//!   `recv` keys that `Split()` produced, so the two directions use different keys. That matters
//!   on this link more than most: the radio is half duplex, and Noise §12.2 calls using one
//!   cipherstate for both directions "a catastrophic security failure" the moment the two ends do
//!   not strictly alternate. We do not, and we cannot: separate keys make the case impossible.
//! - **The nonce is the record counter, and it never repeats.** A retransmission is sealed again
//!   with a fresh counter (the L2 link deduplicates by content hash, so a byte-identical resend
//!   would be dropped as a duplicate), and [`record::seal`] refuses `u32::MAX`, so a key is never
//!   used twice with one nonce.
//! - **Rekey is a new handshake, not `Rekey()`.** Noise's `Rekey()` replaces the key but keeps the
//!   nonce, which is a trap: the counter would restart at a value the old key already used.
//!   [`Session::needs_rekey`](crate::Session::needs_rekey) instead makes the caller run a fresh
//!   handshake, so keys *and* counters are new together.
//! - **The filter tag is constant-time.** `verify_truncated_left` compares in constant time, and
//!   the PSK never leaves [`snow`]: the filter key is `HMAC(psk, "ldmux-mesh filter v1")`, a
//!   domain-separated derivation, not the PSK itself.
//! - **The prologue binds the suite** ([`prologue`]), so mismatched implementations fail the
//!   handshake instead of deriving different keys from the same flags.
//! - **The clear header is not authenticated, and does not need to be.** Kind and session index
//!   select a key and a counter selects the nonce, so any change to them fails the tag. The tag is
//!   never truncated (16 bytes) and the replay window advances only after it verifies.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use snow::{Builder, HandshakeState, StatelessTransportState};
use zeroize::Zeroize;

pub const SUITE: &str = "Noise_IKpsk2_25519_ChaChaPoly_SHA256";
pub const KIND_MSG1: u8 = 0x11;
pub const KIND_MSG2: u8 = 0x12;
pub const MSG1_LEN: usize = 121;
pub const MSG2_LEN: usize = 50;
pub const FILTER_TAG: usize = 16;
/// Domain separation, and the suite is part of it.
///
/// `Noise IKpsk2` with a PSK is only as separate from another `Noise IKpsk2` deployment as its
/// prologue and its PSK make it, and the PSK here is per pairing. Mixing the suite name into the
/// prologue is what makes a change of suite a handshake failure between mismatched peers instead
/// of two implementations silently deriving different keys from the same flags — the rollback
/// problem the Noise spec answers with "fold negotiation data into the prologue".
const PROLOGUE: &str = concat!("ldmux-mesh-session-v1:", "Noise_IKpsk2_25519_ChaChaPoly_SHA256");
const FILTER_LABEL: &[u8] = b"ldmux-mesh filter v1";
/// ADR-0020: default replay window for the handshake timestamp, 5 minutes.
pub const TIMESTAMP_WINDOW_MS: u64 = 5 * 60 * 1000;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum HandshakeError {
    Malformed,
    /// No pairing's PSK produced this filter tag. Cost: one MAC per pairing.
    BadFilter,
    /// The timestamp is outside the window around now.
    Stale,
    /// The timestamp is not greater than the last accepted one for this pairing.
    Replay,
    /// The PSK matched but a different phone key presented it.
    PinMismatch,
    Crypto,
}

/// An X25519 keypair. The private half is wiped on drop.
pub struct Keypair {
    pub private: [u8; 32],
    pub public: [u8; 32],
}

impl Drop for Keypair {
    fn drop(&mut self) {
        self.private.zeroize();
    }
}

impl Keypair {
    pub fn generate() -> Result<Self, HandshakeError> {
        let kp = Builder::new(params())
            .generate_keypair()
            .map_err(|_| HandshakeError::Crypto)?;
        let private: [u8; 32] = kp
            .private
            .as_slice()
            .try_into()
            .map_err(|_| HandshakeError::Crypto)?;
        let public: [u8; 32] = kp
            .public
            .as_slice()
            .try_into()
            .map_err(|_| HandshakeError::Crypto)?;
        Ok(Self { private, public })
    }

    /// Rebuild a stored keypair. Both halves are stored: `snow` exposes no way to derive the
    /// public half from the private one, and ADR-0023 deliberately adds no curve crate for it.
    #[must_use]
    pub fn from_parts(private: [u8; 32], public: [u8; 32]) -> Self {
        Self { private, public }
    }
}

fn params() -> snow::params::NoiseParams {
    // The suite string is a constant; a parse failure is a build-time bug, not input.
    SUITE.parse().expect("the suite string is valid")
}

/// An established session's keys. Opaque outside this crate except for who the peer is.
pub struct Established {
    pub(crate) transport: StatelessTransportState,
    /// The index the responder assigned. Carried in the clear on every record.
    pub index: u8,
    /// The peer's static public key.
    pub remote_static: [u8; 32],
    pub is_initiator: bool,
}

impl Established {
    /// The transport keys, for the record-layer tests. Not part of the supported surface.
    #[doc(hidden)]
    #[must_use]
    pub fn transport_for_tests(&self) -> &StatelessTransportState {
        &self.transport
    }
}

fn filter_key(psk: &[u8; 32]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(psk).expect("hmac accepts any key length");
    mac.update(FILTER_LABEL);
    mac.finalize().into_bytes().into()
}

fn filter_mac(key: &[u8; 32], kind: u8, noise: &[u8]) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(&[kind]);
    mac.update(noise);
    mac
}

/// The phone's side. One handshake, then [`Self::finish`].
pub struct Initiator {
    hs: HandshakeState,
    remote_static: [u8; 32],
}

impl Initiator {
    /// Build message 1. `timestamp_ms` is wall-clock Unix milliseconds.
    pub fn start(
        local: &Keypair,
        remote_public: &[u8; 32],
        psk: &[u8; 32],
        timestamp_ms: u64,
    ) -> Result<(Self, Vec<u8>), HandshakeError> {
        let mut hs = Builder::new(params())
            .local_private_key(&local.private)
            .and_then(|b| b.remote_public_key(remote_public))
            .and_then(|b| b.psk(2, psk))
            .and_then(|b| b.prologue(PROLOGUE.as_bytes()))
            .and_then(Builder::build_initiator)
            .map_err(|_| HandshakeError::Crypto)?;
        let mut noise = [0u8; 256];
        let n = hs
            .write_message(&timestamp_ms.to_be_bytes(), &mut noise)
            .map_err(|_| HandshakeError::Crypto)?;
        let mut key = filter_key(psk);
        let tag = filter_mac(&key, KIND_MSG1, &noise[..n])
            .finalize()
            .into_bytes();
        key.zeroize();
        let mut msg = Vec::with_capacity(MSG1_LEN);
        msg.push(KIND_MSG1);
        msg.extend_from_slice(&tag[..FILTER_TAG]);
        msg.extend_from_slice(&noise[..n]);
        debug_assert_eq!(msg.len(), MSG1_LEN);
        Ok((
            Self {
                hs,
                remote_static: *remote_public,
            },
            msg,
        ))
    }

    /// Consume message 2. Application bytes may flow only after this succeeds.
    pub fn finish(mut self, msg2: &[u8]) -> Result<Established, HandshakeError> {
        if msg2.len() != MSG2_LEN || msg2[0] != KIND_MSG2 {
            return Err(HandshakeError::Malformed);
        }
        let mut payload = [0u8; 64];
        let n = self
            .hs
            .read_message(&msg2[1..], &mut payload)
            .map_err(|_| HandshakeError::Crypto)?;
        if n != 1 {
            return Err(HandshakeError::Malformed);
        }
        let transport = self
            .hs
            .into_stateless_transport_mode()
            .map_err(|_| HandshakeError::Crypto)?;
        Ok(Established {
            transport,
            index: payload[0],
            remote_static: self.remote_static,
            is_initiator: true,
        })
    }
}

/// What the host knows about one paired phone.
#[derive(Clone, Debug)]
pub struct PairingEntry {
    pub psk: [u8; 32],
    /// The phone key this pairing is pinned to, once a handshake has succeeded.
    pub pinned: Option<[u8; 32]>,
    /// The last handshake timestamp accepted for this pairing.
    pub last_timestamp_ms: u64,
}

/// The result of an accepted message 1.
pub struct Accepted {
    /// Message 2, to send back.
    pub reply: Vec<u8>,
    pub established: Established,
    /// Index into the `pairings` slice that matched.
    pub pairing: usize,
    /// To persist: the phone key to pin, and the timestamp to remember.
    pub remote_static: [u8; 32],
    pub timestamp_ms: u64,
}

/// The host's side.
pub struct Responder {
    pub static_key: Keypair,
    pub window_ms: u64,
}

impl Responder {
    pub fn new(static_key: Keypair) -> Self {
        Self {
            static_key,
            window_ms: TIMESTAMP_WINDOW_MS,
        }
    }

    /// Check order is the cheap-drop order of ADR-0020 section 5: shape, then the filter tag
    /// (one MAC per pairing), then Diffie-Hellman, then the timestamp window and monotonicity,
    /// then the pin.
    pub fn accept(
        &self,
        pairings: &[PairingEntry],
        now_ms: u64,
        msg1: &[u8],
        index: u8,
    ) -> Result<Accepted, HandshakeError> {
        if msg1.len() != MSG1_LEN || msg1[0] != KIND_MSG1 {
            return Err(HandshakeError::Malformed);
        }
        let tag = &msg1[1..=FILTER_TAG];
        let noise = &msg1[1 + FILTER_TAG..];
        let mut matched = None;
        for (i, p) in pairings.iter().enumerate() {
            let mut key = filter_key(&p.psk);
            let ok = filter_mac(&key, KIND_MSG1, noise)
                .verify_truncated_left(tag)
                .is_ok();
            key.zeroize();
            if ok {
                matched = Some(i);
                break;
            }
        }
        let i = matched.ok_or(HandshakeError::BadFilter)?;
        let entry = &pairings[i];

        let mut hs = Builder::new(params())
            .local_private_key(&self.static_key.private)
            .and_then(|b| b.psk(2, &entry.psk))
            .and_then(|b| b.prologue(PROLOGUE.as_bytes()))
            .and_then(Builder::build_responder)
            .map_err(|_| HandshakeError::Crypto)?;
        let mut payload = [0u8; 64];
        let n = hs
            .read_message(noise, &mut payload)
            .map_err(|_| HandshakeError::Crypto)?;
        let ts_bytes: [u8; 8] = payload[..n]
            .try_into()
            .map_err(|_| HandshakeError::Malformed)?;
        let timestamp_ms = u64::from_be_bytes(ts_bytes);
        if now_ms.abs_diff(timestamp_ms) > self.window_ms {
            return Err(HandshakeError::Stale);
        }
        if timestamp_ms <= entry.last_timestamp_ms {
            return Err(HandshakeError::Replay);
        }
        let remote_static: [u8; 32] = hs
            .get_remote_static()
            .and_then(|s| s.try_into().ok())
            .ok_or(HandshakeError::Crypto)?;
        if entry.pinned.is_some_and(|pin| pin != remote_static) {
            return Err(HandshakeError::PinMismatch);
        }
        let mut out = [0u8; 128];
        let n = hs
            .write_message(&[index], &mut out)
            .map_err(|_| HandshakeError::Crypto)?;
        let mut reply = Vec::with_capacity(MSG2_LEN);
        reply.push(KIND_MSG2);
        reply.extend_from_slice(&out[..n]);
        debug_assert_eq!(reply.len(), MSG2_LEN);
        let transport = hs
            .into_stateless_transport_mode()
            .map_err(|_| HandshakeError::Crypto)?;
        Ok(Accepted {
            reply,
            established: Established {
                transport,
                index,
                remote_static,
                is_initiator: false,
            },
            pairing: i,
            remote_static,
            timestamp_ms,
        })
    }
}
