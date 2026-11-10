//! One companion session: the handshake, the two identity commands, raw sends,
//! and the receive log.
//!
//! The protocol is strictly **one command at a time**. It has no command ids on
//! the wire, so a second command sent while the first is unanswered leaves the
//! client unable to tell which reply is which. This session therefore serialises
//! by construction: every [`Companion::request`] sends, waits, and returns before
//! the next call can send.
//!
//! The other rule is about the radio's boot. Opening the port can reset an
//! ESP32-S3, so a command written in the first second may be swallowed by the
//! boot. A request is therefore re-sent only while the radio has said *nothing at
//! all*; once any byte arrives, re-sending would turn one command into two.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crate::codec::{
    APP_TARGET_VERSION, AdvertPath, CodecError, CoreStats, DeviceInfo, FloodScope, RadioStats,
    SelfInfo, build_default_flood_scope, build_raw_direct, codes, commands, parse_advert_path,
    parse_core_stats, parse_default_flood_scope, parse_device_info, parse_radio_stats,
    parse_self_info,
};
use crate::framing::{FrameError, FrameReader, encode};
use crate::link::ByteLink;

/// How long to wait before re-sending a command the radio has not acknowledged.
pub const RESEND_INTERVAL: Duration = Duration::from_millis(600);

/// How long the periodic route read waits for the radio's cache lookup.
///
/// The answer is one round trip on a link that is working, and this call sits on the same thread
/// that serves typed records, so its timeout is a bound on how long a keystroke can wait behind it.
/// A radio that does not answer is retried by the caller's own cadence, so nothing is lost by
/// giving up quickly.
pub const ROUTE_READ_TIMEOUT: Duration = Duration::from_millis(1500);

/// The most a read is allowed to block before the deadline is checked again.
const POLL_TIMEOUT_MS: u16 = 20;

#[derive(Debug)]
pub enum SessionError {
    Io(io::Error),
    Frame(FrameError),
    Codec(CodecError),
    /// Nothing at all came back for the expected reply.
    Timeout { expected: u8, waited: Duration },
    /// The radio answered with `RESP_ERROR`, naming a code.
    Rejected { code: u8, meaning: &'static str },
    /// A different response arrived where the requested one was expected.
    Unexpected { got: u8 },
    /// The firmware answered `RESP_CODE_DISABLED`: it is in another state and cannot serve this
    /// command now (a radio mid-retune, or one whose store is busy). Not a fault in the frame, and
    /// not a refusal of the argument either — the same command is worth retrying a moment later.
    Disabled,
    /// A reply that should have carried the fields it was asked for did not.
    Malformed(&'static str),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Io(e) => write!(f, "serial i/o: {e}"),
            SessionError::Frame(e) => write!(f, "framing: {e}"),
            SessionError::Codec(e) => write!(f, "codec: {e}"),
            SessionError::Timeout { expected, waited } => write!(
                f,
                "no reply to 0x{expected:02x} within {:.1}s",
                waited.as_secs_f64()
            ),
            SessionError::Rejected { code, meaning } => {
                write!(f, "radio rejected the command: {meaning} (code {code})")
            }
            SessionError::Unexpected { got } => write!(f, "unexpected reply 0x{got:02x}"),
            SessionError::Disabled => write!(f, "the radio is in another state and cannot answer now"),
            SessionError::Malformed(what) => write!(f, "malformed reply: {what}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<io::Error> for SessionError {
    fn from(e: io::Error) -> Self {
        SessionError::Io(e)
    }
}

impl From<FrameError> for SessionError {
    fn from(e: FrameError) -> Self {
        SessionError::Frame(e)
    }
}

impl From<CodecError> for SessionError {
    fn from(e: CodecError) -> Self {
        SessionError::Codec(e)
    }
}

/// The firmware's error codes, named so a log line says what happened.
pub fn error_meaning(code: u8) -> &'static str {
    match code {
        1 => "UNSUPPORTED_CMD",
        2 => "NOT_FOUND",
        3 => "TABLE_FULL",
        4 => "BAD_STATE",
        5 => "FILE_IO_FAILED",
        6 => "ILLEGAL_ARG",
        _ => "UNKNOWN",
    }
}

/// A live companion session over any [`ByteLink`].
pub struct Companion<L: ByteLink> {
    link: L,
    reader: FrameReader,
    /// Frames that arrived while a request was waiting for its own reply.
    pending: VecDeque<Vec<u8>>,
    pub self_info: Option<SelfInfo>,
    pub device_info: Option<DeviceInfo>,
}

impl<L: ByteLink> Companion<L> {
    pub fn new(link: L) -> Self {
        Self {
            link,
            reader: FrameReader::new(),
            pending: VecDeque::new(),
            self_info: None,
            device_info: None,
        }
    }

    pub fn link_mut(&mut self) -> &mut L {
        &mut self.link
    }

    /// Register this client and read the radio's identity.
    ///
    /// The payload matches the phone: `0x01`, seven reserved bytes, then the app
    /// name. The firmware latches the client's protocol version from
    /// `DEVICE_QUERY`, not from this frame, so the reserved bytes stay zero.
    pub fn app_start(&mut self, app_name: &str) -> Result<SelfInfo, SessionError> {
        let mut payload = Vec::with_capacity(9 + app_name.len());
        payload.push(commands::APP_START);
        payload.extend_from_slice(&[0u8; 7]);
        payload.extend_from_slice(app_name.as_bytes());
        let reply = self.request(&payload, codes::SELF_INFO, Duration::from_secs(5))?;
        let info = parse_self_info(&reply)?;
        self.self_info = Some(info.clone());
        Ok(info)
    }

    /// Ask what the firmware can do, claiming protocol version 3.
    ///
    /// The version is not decoration: the firmware stores it and gates its V3
    /// message frames on it, and those are the frames that carry SNR.
    pub fn device_query(&mut self) -> Result<DeviceInfo, SessionError> {
        let reply = self.request(
            &[commands::DEVICE_QUERY, APP_TARGET_VERSION],
            codes::DEVICE_INFO,
            Duration::from_secs(5),
        )?;
        let info = parse_device_info(&reply).ok_or(SessionError::Malformed("device info"))?;
        self.device_info = Some(info.clone());
        Ok(info)
    }

    /// What the radio measured on the air, and what it has spent there.
    ///
    /// **The only ground truth about the link.** A serial companion reports the noise floor, the
    /// RSSI and SNR of the last packet it heard, and its own airtime counters — so "is this link
    /// reaching" is answered by the radio instead of guessed from the application's records, and the
    /// daemon's own meter has something to be checked against. One serial command, no airtime.
    pub fn radio_stats(&mut self) -> Result<RadioStats, SessionError> {
        let reply = self.request(
            &[commands::GET_STATS, codes::STATS_TYPE_RADIO],
            codes::STATS,
            Duration::from_millis(1_500),
        )?;
        parse_radio_stats(&reply).map_err(|_| SessionError::Malformed("radio stats"))
    }

    /// The radio's battery, uptime, error flags and outbound queue depth.
    pub fn core_stats(&mut self) -> Result<CoreStats, SessionError> {
        let reply = self.request(
            &[commands::GET_STATS, codes::STATS_TYPE_CORE],
            codes::STATS,
            Duration::from_millis(1_500),
        )?;
        parse_core_stats(&reply).map_err(|_| SessionError::Malformed("core stats"))
    }

    /// Emit this radio's advert, flooded so repeaters forward it.
    ///
    /// The reply means "the firmware queued it", not "a peer heard it": an advert is a broadcast
    /// with no acknowledgement, which is why the path it teaches can take minutes to appear.
    pub fn send_self_advert(&mut self, flood: bool) -> Result<(), SessionError> {
        self.send_self_advert_within(flood, Duration::from_secs(10))
    }

    /// The same, with a bounded wait for the radio's answer.
    ///
    /// **A radio that stopped answering must not hold the serving loop.** The firmware acknowledges
    /// `SEND_SELF_ADVERT` from its own queue, so two seconds is already a hundred times what the
    /// answer needs, and the ten-second form blocked the one loop that also has to read received
    /// frames — every peer record arriving during that wait queued behind it.
    pub fn send_self_advert_within(
        &mut self,
        flood: bool,
        timeout: Duration,
    ) -> Result<(), SessionError> {
        let mode = u8::from(flood);
        self.request(&[commands::SEND_SELF_ADVERT, mode], codes::OK, timeout)?;
        Ok(())
    }

    /// Read the route a recently heard advert took, from the radio's advert-path cache.
    ///
    /// **The request carries the whole 32-byte public key**, even though the cache is keyed by the
    /// first seven bytes: the firmware's guard is `len >= PUB_KEY_SIZE + 2` and it compares
    /// `sizeof(AdvertPath::pubkey_prefix)` — seven — bytes of that key (`MyMesh.cpp:1851-1858`). A
    /// short prefix frame is answered `ERR_CODE_UNSUPPORTED_CMD` and no route is ever learned.
    ///
    /// The reply carries the path in travel order towards this radio; a caller sending *to* that
    /// node reverses the hop groups (`PeerPath::reversed`). A radio that has not heard the node
    /// recently answers `ERR_CODE_NOT_FOUND`, which arrives here as [`SessionError::Rejected`] — no
    /// route, not a failure.
    pub fn get_advert_path(&mut self, public_key: &[u8; 32]) -> Result<AdvertPath, SessionError> {
        let mut payload = Vec::with_capacity(34);
        payload.push(commands::GET_ADVERT_PATH);
        payload.push(0); // reserved
        payload.extend_from_slice(public_key);
        // **A cache lookup, so the answer is one round trip.** The firmware reads a 16-slot RAM
        // table and answers at once (`MyMesh.cpp:1851-1869`); the five seconds this used to wait
        // were five seconds of the radio loop not serving a typed record, on the one path the
        // operator feels as "typing stalls" — and the wedged radio it guarded against is also the
        // case where the caller repeats the read on its own cadence. A late answer is not lost: it
        // is left in `pending` and taken by `next_push`.
        let reply = self.request(&payload, codes::ADVERT_PATH, ROUTE_READ_TIMEOUT)?;
        parse_advert_path(&reply).ok_or(SessionError::Malformed("advert path"))
    }

    /// Transmit one record as a raw direct packet, building the packet here.
    ///
    /// The reply means "the radio accepted it for transmission", never "the peer
    /// received it": delivery is the session layer's problem, above this one.
    pub fn send_raw(&mut self, hash_size: u8, path: &[u8], record: &[u8]) -> Result<(), SessionError> {
        self.send_raw_within(hash_size, path, record, Duration::from_secs(10))
    }

    /// The same, with a deadline of the caller's choosing.
    ///
    /// A **route reply** is 40-ish bytes and the radio accepts or refuses it at once; waiting ten
    /// seconds for its `OK` holds the one answer window — and therefore every record behind it —
    /// for ten seconds on a radio that has stopped answering. The short form is for those sends.
    pub fn send_raw_within(
        &mut self,
        hash_size: u8,
        path: &[u8],
        record: &[u8],
        timeout: Duration,
    ) -> Result<(), SessionError> {
        let packet = build_raw_direct(hash_size, path, record)?;
        self.send_packet_within(&packet, timeout)
    }

    /// Transmit a packet we built, whatever its route and payload type.
    ///
    /// **This is what makes an on-demand route request possible.** A raw record can only be direct
    /// (`Mesh.cpp`, `case PAYLOAD_TYPE_RAW_CUSTOM`), so a route request has to be a different
    /// payload type with the FLOOD route, and `CMD_SEND_RAW_PACKET` takes a whole packet: the
    /// firmware parses it with `tryParsePacket` and queues it without looking at the payload
    /// (`MyMesh.cpp:2003-2016`, `Dispatcher::sendPacket`). The frame ceiling is the firmware's
    /// own: two bytes of command and priority out of `MAX_FRAME_SIZE` 176, so 174 of packet.
    pub fn send_packet(&mut self, packet: &[u8]) -> Result<(), SessionError> {
        self.send_packet_within(packet, Duration::from_secs(10))
    }

    /// The same, with a deadline of the caller's choosing.
    pub fn send_packet_within(
        &mut self,
        packet: &[u8],
        timeout: Duration,
    ) -> Result<(), SessionError> {
        if packet.len() > crate::codec::PACKET_MAX {
            return Err(SessionError::Codec(crate::codec::CodecError::TooLong {
                len: packet.len(),
                max: crate::codec::PACKET_MAX,
            }));
        }
        let mut payload = Vec::with_capacity(2 + packet.len());
        payload.push(commands::SEND_RAW_PACKET);
        payload.push(0); // priority
        payload.extend_from_slice(packet);
        self.request(&payload, codes::OK, timeout)?;
        Ok(())
    }

    /// Set the radio's clock from this machine's wall clock.
    ///
    /// One command, best effort: the firmware refuses a time earlier than the one it holds, so a
    /// repeated call within a second is answered with an error and nothing changes. What it buys is
    /// that every advert the radio hears is stamped with real time, which is what makes a route's
    /// age knowable (`AdvertPath::received_at`).
    pub fn set_time(&mut self, unix_seconds: u32) -> Result<(), SessionError> {
        let mut payload = Vec::with_capacity(5);
        payload.push(commands::SET_DEVICE_TIME);
        payload.extend_from_slice(&unix_seconds.to_le_bytes());
        self.request(&payload, codes::OK, Duration::from_secs(5))?;
        Ok(())
    }

    /// Read the radio's **default flood scope**, or `None` when it has none.
    ///
    /// The reply is `[0x1C][name 31][key 16]`, and the code alone is the firmware's "null" answer
    /// (`MyMesh.cpp:1964-1972`). The key comes back with the name, which is the only way a private
    /// (`$…`) region's key can be known: a public one is derivable from the name, a private one is
    /// not.
    pub fn get_default_flood_scope(&mut self) -> Result<Option<FloodScope>, SessionError> {
        let reply = self.request(
            &[commands::GET_DEFAULT_FLOOD_SCOPE],
            codes::DEFAULT_FLOOD_SCOPE,
            ROUTE_READ_TIMEOUT,
        )?;
        parse_default_flood_scope(&reply).ok_or(SessionError::Malformed("default flood scope"))
    }

    /// Write the radio's default flood scope, or clear it with `None`.
    ///
    /// **ldmux only calls this with a scope it learned from the air while the radio had none**, so
    /// the operator's own choice is never overwritten; the radio's default scope is what its
    /// self-adverts use, and a daemon that knew a scope the radio did not would otherwise advertise
    /// unscoped on a mesh that drops unscoped floods.
    pub fn set_default_flood_scope(&mut self, scope: Option<&FloodScope>) -> Result<(), SessionError> {
        let frame = build_default_flood_scope(scope)?;
        self.request(&frame, codes::OK, Duration::from_secs(5))?;
        Ok(())
    }

    /// Set the path-hash width this radio's own adverts use.
    ///
    /// `mode` is the firmware's index: 0 = one byte, 1 = two, 2 = three (`MyMesh.cpp:1466-1473`).
    /// The radio treats this as a preference about its *own* origin packets; it forwards whatever
    /// width arrives either way.
    pub fn set_path_hash_mode(&mut self, mode: u8) -> Result<(), SessionError> {
        let payload = vec![commands::SET_PATH_HASH_MODE, 0, mode];
        self.request(&payload, codes::OK, Duration::from_secs(5))?;
        Ok(())
    }

    /// Set the radio's transmit power in dBm.
    ///
    /// `CMD_SET_RADIO_TX_POWER` (0x0C) is **one signed byte** (`MyMesh.cpp:1386-1396`), and the
    /// firmware's window is -9 to the board's maximum, refusing anything else with
    /// `ILLEGAL_ARG`. It is a global, persisted setting of the radio, like the tuple, so a phone
    /// that sets it and a host that sets it are writing the same register — last writer wins.
    pub fn set_tx_power(&mut self, dbm: i8) -> Result<(), SessionError> {
        self.request(
            &[commands::SET_RADIO_TX_POWER, dbm as u8],
            codes::OK,
            Duration::from_secs(5),
        )?;
        Ok(())
    }

    /// Tune the radio to a tuple, and confirm the firmware took it.
    ///
    /// Frequencies are in kHz and bandwidths in Hz, which is what the command carries; the
    /// firmware's own bounds (150000..2500000 kHz, 7000..500000 Hz, SF 5..12, CR 5..8) are its
    /// business, and a refusal comes back as an error rather than a silently unchanged radio.
    /// Tune the radio, **keeping the client-repeat flag it already has**.
    ///
    /// The command's sixth optional byte is persisted as `_prefs.setRepeatEn(repeat != 0)`
    /// (`MyMesh.cpp:1397-1410`), so a frame that stops after the coding rate silently clears an
    /// operator's repeat setting on every preset write. `repeat` is that byte: `None` leaves the
    /// firmware's stored value alone, `Some` sets it, and the caller passes what
    /// [`crate::codec::DeviceInfo::repeat_enabled`] reported — see [`Self::set_radio_params_keeping`].
    pub fn set_radio_params(
        &mut self,
        freq_khz: u32,
        bw_hz: u32,
        sf: u8,
        cr: u8,
    ) -> Result<(), SessionError> {
        self.set_radio_params_keeping(freq_khz, bw_hz, sf, cr, None)
    }

    /// The same, with the radio's client-repeat flag carried through the write.
    pub fn set_radio_params_keeping(
        &mut self,
        freq_khz: u32,
        bw_hz: u32,
        sf: u8,
        cr: u8,
        repeat: Option<bool>,
    ) -> Result<(), SessionError> {
        let mut payload = Vec::with_capacity(12);
        payload.push(commands::SET_RADIO_PARAMS);
        payload.extend_from_slice(&freq_khz.to_le_bytes());
        payload.extend_from_slice(&bw_hz.to_le_bytes());
        payload.push(sf);
        payload.push(cr);
        if let Some(repeat) = repeat {
            payload.push(u8::from(repeat));
        }
        self.request(&payload, codes::OK, Duration::from_secs(10))?;
        Ok(())
    }

    /// Whether `code` is a response this crate reads, wherever it is expected or not.
    ///
    /// Used to tell "a late answer to an earlier command" (keep it, it is a fact about the radio)
    /// from "the firmware is speaking a protocol this client does not know" (a real framing fault).
    fn is_known_response(code: u8) -> bool {
    matches!(
            code,
            codes::OK
                | codes::SELF_INFO
                | codes::DEVICE_INFO
                | codes::ADVERT_PATH
                | codes::DEFAULT_FLOOD_SCOPE
                | codes::STATS
                | codes::CONTACT_START
                | codes::NO_MORE_MESSAGES
                | codes::CHANNEL_MSG_RECV
                | codes::CHANNEL_MSG_RECV_V3
        )
    }


/// The next asynchronous frame, if one arrives within `timeout`.
    ///
    /// Pushes are the receive log (`0x88`) and the advert/ack/status family. A
    /// caller that only wants packets calls this in a loop and ignores the rest.
    /// Whether a frame is waiting to be read, so a caller can ask before adding work.
    ///
    /// **A serial command and an inbound packet share one port.** Polling the radio's stats while a
    /// received packet is already queued made the poll wait out its own timeout behind that packet
    /// (and every retry after it), so a caller on the idle path asks this first: with a frame in
    /// hand, the frame wins.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn next_push(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, SessionError> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(Some(frame));
        }
        let deadline = Instant::now() + timeout;
        loop {
            self.pump(POLL_TIMEOUT_MS)?;
            if let Some(frame) = self.pending.pop_front() {
                return Ok(Some(frame));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    /// Send one command and wait for the reply with `expect` as its first byte.
    fn request(
        &mut self,
        payload: &[u8],
        expect: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>, SessionError> {
        let framed = encode(payload)?;
        let start = Instant::now();
        let deadline = start + timeout;
        let mut last_send = start;
        let mut heard_anything = false;
        self.link.write_all(&framed)?;
        loop {
            // Any byte from the radio — a frame, a boot banner, `Saving prefs…` — is the radio
            // being alive, and the resend below keys on silence.
            if self.pump(POLL_TIMEOUT_MS)? {
                heard_anything = true;
            }
            // Pushes that arrive while we wait for the reply are set aside rather
            // than pushed back into the queue: re-queueing them inside the loop
            // would pop and re-push the same frame forever.
            let mut pushes: Vec<Vec<u8>> = Vec::new();
            let mut outcome: Option<Result<Vec<u8>, SessionError>> = None;
            while let Some(frame) = self.pending.pop_front() {
                match frame.first().copied() {
                    Some(code) if code == expect => {
                        outcome = Some(Ok(frame));
                        break;
                    }
                    Some(codes::DISABLED) => {
                        // **A state, not a fault.** The firmware answers this when it is mid-retune or
                        // its store is busy (`RESP_CODE_DISABLED`, MyMesh.cpp:86), and the same
                        // command is worth sending again — where `Unexpected` made it a framing bug
                        // that aborted the whole ladder.
                        outcome = Some(Err(SessionError::Disabled));
                        break;
                    }
                    Some(codes::ERROR) => {
                        let code = frame.get(1).copied().unwrap_or(0);
                        outcome = Some(Err(SessionError::Rejected {
                            code,
                            meaning: error_meaning(code),
                        }));
                        break;
                    }
                    Some(code) if code >= 0x80 => {
                        // An asynchronous push, not the reply. Keep it for the
                        // caller and keep waiting.
                        pushes.push(frame);
                        heard_anything = true;
                    }
                    // **A late reply belongs to a command that already gave up, and it must not
                    // abort this one.** The advert-path read has a 1.5 s deadline; a radio that
                    // answers at 1.6 s used to make the *next* command (a record send) fail with
                    // "unexpected response", which cost an ARQ attempt and its airtime for a frame
                    // nobody was waiting for any more. A code this crate reads is kept for
                    // `next_push`; only a code nothing here understands is still a framing fault.
                    Some(got) if Self::is_known_response(got) => {
                        pushes.push(frame);
                    }
                    Some(got) => {
                        outcome = Some(Err(SessionError::Unexpected { got }));
                        break;
                    }
                    None => {}
                }
            }
            for push in pushes {
                self.pending.push_back(push);
            }
            if let Some(result) = outcome {
                return result;
            }
            if Instant::now() >= deadline {
                return Err(SessionError::Timeout { expected: expect, waited: timeout });
            }
            if !heard_anything && last_send.elapsed() >= RESEND_INTERVAL {
                // Only while the radio is completely silent: a re-sent command
                // that mutates state is a second operation.
                self.link.write_all(&framed)?;
                last_send = Instant::now();
            }
        }
    }

    /// Read whatever the port has, decode whole frames into `pending`.
    ///
    /// Returns whether any bytes arrived at all, **including console text**. A boot banner or
    /// `Saving prefs…` is the radio being alive, and the resend rule in [`Self::request`] keys on
    /// silence; counting only push *frames* made the client re-send a mutating command every
    /// 600 ms while the firmware was still printing its banner.
    fn pump(&mut self, poll_timeout_ms: u16) -> Result<bool, SessionError> {
        let mut buf = [0u8; 512];
        let mut frames = Vec::new();
        let started = Instant::now();
        let mut heard = false;
        loop {
            let n = self.link.read(&mut buf)?;
            if n > 0 {
                heard = true;
                self.reader.feed(&buf[..n], &mut frames);
                for frame in frames.drain(..) {
                    self.pending.push_back(frame);
                }
            }
            if n == 0 || started.elapsed() >= Duration::from_millis(poll_timeout_ms as u64) {
                return Ok(heard);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::codes;
    use crate::link::MemoryLink;

    fn self_info_payload() -> Vec<u8> {
        let mut v = vec![codes::SELF_INFO, 1, 22, 22];
        v.extend((1..=32).map(|i| i as u8));
        v.extend(0i32.to_le_bytes());
        v.extend(0i32.to_le_bytes());
        v.extend([0u8, 1, 0, 0]);
        v.extend(914_250u32.to_le_bytes());
        v.extend(500_000u32.to_le_bytes());
        v.extend([8u8, 5]);
        v.extend(b"HELTEC-BASE");
        v
    }

    /// **The one route a stock radio can give.** The request is the command, a reserved byte and the
    /// **whole 32-byte key** (the firmware's guard is `len >= PUB_KEY_SIZE + 2` and it compares seven
    /// bytes of that key); the answer carries the firmware's encoded path length, which is hop groups,
    /// not a byte count.
    #[test]
    fn an_advert_path_round_trips() {
        let mut link = MemoryLink::new();
        let mut reply = vec![codes::ADVERT_PATH];
        reply.extend(0x11_22_33_44u32.to_le_bytes());
        reply.push(0x42); // two-byte hashes, two hops
        reply.extend([0xA1, 0xA2, 0xB1, 0xB2]);
        link.push_payload(&reply);
        let mut companion = Companion::new(link);
        let path = companion.get_advert_path(&[7u8; 32]).expect("advert path");
        assert_eq!(path.hash_size, 2);
        assert_eq!(path.hops, 2);
        assert_eq!(path.bytes, vec![0xA1, 0xA2, 0xB1, 0xB2]);
        assert_eq!(path.received_at, 0x1122_3344);
        // What went out: the command, a zero, then the whole 32-byte key — the firmware's guard is
        // `len >= PUB_KEY_SIZE + 2`, so a short prefix frame is answered "unsupported command".
        let written = &companion.link_mut().written;
        let body = &written[3..];
        assert_eq!(body[0], commands::GET_ADVERT_PATH);
        assert_eq!(body[1], 0);
        assert_eq!(&body[2..34], &[7u8; 32]);
        assert_eq!(body.len(), 34);
    }

    #[test]
    fn app_start_reads_the_radios_identity() {
        let mut link = MemoryLink::new();
        link.push_payload(&self_info_payload());
        let mut companion = Companion::new(link);
        let info = companion.app_start("ldmux").expect("self info");
        assert_eq!(info.name, "HELTEC-BASE");
        assert_eq!(info.radio_freq_mhz, 914.25);
        // The frame it actually wrote: 0x01, seven zeros, then the app name.
        let written = &companion.link_mut().written;
        assert_eq!(written[0], 0x3C);
        assert_eq!(written[3], commands::APP_START);
        assert_eq!(&written[4..11], &[0u8; 7]);
        assert_eq!(&written[11..], b"ldmux");
    }

    #[test]
    fn device_query_claims_version_three() {
        let mut link = MemoryLink::new();
        link.push_payload(&[codes::DEVICE_INFO, 13, 175, 40]);
        let mut companion = Companion::new(link);
        companion.device_query().expect("device info");
        let written = &companion.link_mut().written;
        assert_eq!(written[3], commands::DEVICE_QUERY);
        assert_eq!(written[4], 3, "the firmware gates its V3 frames on this byte");
    }

    #[test]
    fn a_rejection_names_the_code() {
        let mut link = MemoryLink::new();
        link.push_payload(&[codes::ERROR, 2]);
        let mut companion = Companion::new(link);
        let err = companion.device_query().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("NOT_FOUND"), "got: {text}");
    }

    #[test]
    fn silence_is_a_timeout_not_an_empty_reply() {
        let mut companion = Companion::new(MemoryLink::new());
        let err = companion
            .app_start("ldmux")
            .expect_err("a silent radio must not read as success");
        assert!(matches!(err, SessionError::Timeout { .. }), "got: {err}");
    }

    #[test]
    fn a_push_arriving_mid_request_is_kept_for_the_caller() {
        let mut link = MemoryLink::new();
        link.push_payload(&[codes::LOG_RX_DATA, 0xE0, 0xA1, 0x12, 0x00]);
        link.push_payload(&self_info_payload());
        let mut companion = Companion::new(link);
        companion.app_start("ldmux").expect("self info survives a push");
        let push = companion.next_push(Duration::from_millis(50)).unwrap();
        assert_eq!(push.unwrap()[0], codes::LOG_RX_DATA);
    }

    /// **Console text is the radio being alive.** The resend rule exists for a silent port; a
    /// boot banner or `Saving prefs…` used to leave `heard_anything` false, so a mutating command
    /// was written again every 600 ms while the firmware was still printing.
    #[test]
    fn console_text_counts_as_hearing_the_radio_and_stops_a_resend() {
        let mut link = MemoryLink::new();
        link.push_bytes(b"Saving prefs...\r\n");
        let mut companion = Companion::new(link);
        let frame = encode(&[commands::APP_START]).unwrap();
        // Longer than `RESEND_INTERVAL`: a silent port would be written twice.
        let _ = companion.request(
            &[commands::APP_START],
            codes::SELF_INFO,
            Duration::from_millis(900),
        );
        assert_eq!(
            companion.link_mut().written,
            frame,
            "one write: the banner proved the radio was there"
        );
    }

    #[test]
    fn send_raw_builds_the_packet_and_asks_for_ok() {
        let mut link = MemoryLink::new();
        link.push_payload(&[codes::OK]);
        let mut companion = Companion::new(link);
        companion.send_raw(2, &[0xAA, 0xBB], &[1, 2, 3]).expect("accepted");
        let written = &companion.link_mut().written;
        assert_eq!(written[3], commands::SEND_RAW_PACKET);
        assert_eq!(written[4], 0, "priority");
        assert_eq!(written[5], 0x3E, "raw custom, direct");
        assert_eq!(written[6], 0x41, "one hop of two-byte hashes");
        assert_eq!(&written[7..9], &[0xAA, 0xBB]);
        assert_eq!(&written[9..12], &[1, 2, 3]);
    }
}
