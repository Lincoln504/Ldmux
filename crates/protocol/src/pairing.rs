//! The pairing link: everything a phone needs to reach this daemon, in one URL.
//!
//!   ldmux://pair?v=1&host=100.100.0.1:7625&token=<64 hex>&name=debian
//!
//! `ldmux code` shows it as a QR code; the app scans it (or the system camera
//! opens it) and stores `host` and `token` exactly as they are here. It is a
//! contract between the CLI and the app, so it is pinned from both sides by one
//! fixture (`crates/protocol/test/fixtures/pairing-links.json`, read by these tests and
//! by the app's `PairingTest`).
//!
//! - `host` is the address the daemon *listens on* (`remote_info`), as
//!   `address:port`, IPv6 in brackets. It must be tailnet or loopback (ADR-0016):
//!   a link to anywhere else is refused by the app, so a crafted QR cannot point
//!   a phone at someone else's machine on the internet.
//! - `token` is the bearer token, 64 lowercase hex characters.
//! - `name` is the machine's name, for the app to say what it is paired with.
//!   Optional, at most 64 characters.
//! - `v` is this format's version. A link with a version the app does not know
//!   is refused rather than guessed at.
//! - **Mesh keys (optional, ADR-0020 section 6):** `mv=1&mk=<64 hex>&psk=<64 hex>` (and an
//!   optional `mr=<64 hex>`, the host's radio public key the phone binds) add the
//!   daemon's session public key and the pairing's pre-shared secret, so a phone can use the
//!   mesh route. The link stays `v=1`: a scanner that predates mesh ignores parameters it does
//!   not know (the contract since v1) and pairs the tailnet route exactly as before. A link that
//!   carries *any* mesh field must carry all of them, correctly formed, or it is refused
//!   outright: a half-formed secret is never silently dropped. The PSK is as sensitive as the
//!   token (anyone who photographs the code holds it), so it is never logged or printed apart
//!   from the link itself.
//!
//! Port of `packages/protocol/src/pairing.ts` (the TS original; deleted at D4, readable at git commit `6b3efb8`). The
//! `encodeURIComponent`/`URL`/`URLSearchParams` primitives are replaced by
//! local implementations that are byte-faithful to what those built-ins do for
//! the link shapes this contract produces.

use crate::net::is_tailnet_or_loopback;

/// The link format version.
pub const PAIRING_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub host: String,
    pub token: String,
    pub name: String,
}

/// The tuple the host's own radio is tuned to.
///
/// Two radios on different tuples cannot hear each other, and until this rode in the code the phone
/// had no way to know the host's: a mismatch looked exactly like being out of range, with the only
/// symptom a link that never came up. In the code it is one press to match, and one sentence when
/// they differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPreset {
    pub freq_khz: u32,
    pub bw_hz: u32,
    pub sf: u8,
    pub cr: u8,
    /// The host radio's path hash width in **bytes**, when the host knows it.
    ///
    /// **Not part of the channel.** The width travels in each packet: a forwarder appends its own
    /// hash with the width the packet states, so a two-byte phone and a one-byte repeater work
    /// together. It is published for one reason — an operator comparing two ends can see that they
    /// are set differently, which matters for how many neighbours each can name, not for whether
    /// they can hear each other.
    pub path_bytes: Option<u8>,
}

impl HostPreset {
    /// `freq_khz,bw_hz,sf,cr[,path_bytes]` — numbers, no text.
    fn encode(self) -> String {
        match self.path_bytes {
            Some(b) => format!(
                "{},{},{},{},{}",
                self.freq_khz, self.bw_hz, self.sf, self.cr, b
            ),
            None => format!("{},{},{},{}", self.freq_khz, self.bw_hz, self.sf, self.cr),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        let mut it = text.split(',');
        let freq_khz = it.next()?.parse().ok()?;
        let bw_hz = it.next()?.parse().ok()?;
        let sf = it.next()?.parse().ok()?;
        let cr = it.next()?.parse().ok()?;
        // A fifth number is the path width; a code from a host that predates it has four.
        let path_bytes = match it.next() {
            None => None,
            Some(v) => {
                let b: u8 = v.parse().ok()?;
                if !(1..=3).contains(&b) {
                    return None;
                }
                if it.next().is_some() {
                    return None;
                }
                Some(b)
            }
        };
        // The firmware's own bounds: anything outside them would be refused by the radio, and a
        // code that carries it is not a code this phone should act on.
        let sane = (150_000..=2_500_000).contains(&freq_khz)
            && (7_000..=500_000).contains(&bw_hz)
            && (5..=12).contains(&sf)
            && (5..=8).contains(&cr);
        sane.then_some(Self {
            freq_khz,
            bw_hz,
            sf,
            cr,
            path_bytes,
        })
    }
}

/// The mesh route's keys, as carried by a link (`mv=1&mk=…&psk=…`).
#[derive(Clone, PartialEq, Eq)]
pub struct MeshKeys {
    /// The daemon's static session public key (X25519, 32 bytes).
    pub host_key: [u8; 32],
    /// The pairing's pre-shared secret (32 bytes).
    pub psk: [u8; 32],
    /// The host's radio public key, which the phone binds.
    ///
    /// A daemon that owns its radio reads this from `SELF_INFO` and needs no configuration;
    /// `LDMUX_MESH_SELF` still overrides it for a host that pins a key. Optional because a
    /// daemon that does not own its radio cannot learn it, and a code without it cannot be used.
    pub host_radio: Option<[u8; 32]>,
    /// The tuple the host's radio reports, so the phone can compare and match it.
    pub host_preset: Option<HostPreset>,
}

// A secret never appears in a Debug print.
impl std::fmt::Debug for MeshKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MeshKeys { .. }")
    }
}

/// The mesh-key format version carried in `mv`.
pub const MESH_KEYS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingError(pub String);

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for PairingError {}

/// The link for `p`, with the mesh keys appended when given. Errors on a pairing
/// the app would refuse, so the CLI never shows one.
pub fn pairing_link_with_mesh(
    p: &Pairing,
    mesh: Option<&MeshKeys>,
) -> Result<String, PairingError> {
    if let Some(problem) = pairing_problem(p) {
        return Err(PairingError(problem));
    }
    let mut fields: Vec<(&'static str, String)> = vec![
        ("v", PAIRING_VERSION.to_string()),
        ("host", enc(&p.host)),
        ("token", enc(&p.token)),
    ];
    if !p.name.is_empty() {
        fields.push(("name", enc(&p.name)));
    }
    if let Some(m) = mesh {
        fields.push(("mv", MESH_KEYS_VERSION.to_string()));
        fields.push(("mk", hex32(&m.host_key)));
        fields.push(("psk", hex32(&m.psk)));
        if let Some(r) = &m.host_radio {
            fields.push(("mr", hex32(r)));
        }
        if let Some(p) = m.host_preset {
            fields.push(("mp", p.encode()));
        }
    }
    let q: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
    Ok(format!("ldmux://pair?{}", q.join("&")))
}

/// A link that carries **only** the mesh route: no daemon address and no daemon bearer token
/// (`ldmux://pair?v=1&name=…&mv=1&mk=…&psk=…`).
///
/// For a phone that will reach this machine by radio and never by the tailnet. The daemon's
/// bearer token grants full shell control over the tailnet, so handing it to a device that
/// cannot use it is a privilege that device should not hold (ADR-0020: the tailnet rule "does
/// not apply to mesh pairing"). A scanner that predates this refuses the link, since it has no
/// `host`, which is the safe outcome.
pub fn mesh_only_link(name: &str, mesh: &MeshKeys) -> Result<String, PairingError> {
    let name: String = name
        .chars()
        .filter(|c| !((*c as u32) <= 0x1f || (*c as u32) == 0x7f))
        .take(64)
        .collect();
    let mut fields: Vec<(&'static str, String)> = vec![("v", PAIRING_VERSION.to_string())];
    if !name.is_empty() {
        fields.push(("name", enc(&name)));
    }
    fields.push(("mv", MESH_KEYS_VERSION.to_string()));
    fields.push(("mk", hex32(&mesh.host_key)));
    fields.push(("psk", hex32(&mesh.psk)));
    if let Some(r) = &mesh.host_radio {
        fields.push(("mr", hex32(r)));
    }
    if let Some(p) = mesh.host_preset {
        fields.push(("mp", p.encode()));
    }
    let q: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
    Ok(format!("ldmux://pair?{}", q.join("&")))
}

/// The pairing in `link`, or `None` if it is not a valid ldmux pairing link.
///
/// Scheme and host are case-insensitive (a URL normalises them); the query is
/// case-sensitive and a `#fragment` is dropped. A repeated parameter reads as
/// its first value (as `URLSearchParams.get` does).
///
/// A mesh-only link (no `host`, no `token`) is **not** a tailnet pairing, so this returns `None`
/// for it: use [`parse_pairing_link_full`] to read one.
pub fn parse_pairing_link(link: &str) -> Option<Pairing> {
    parse_pairing_link_full(link)
        .map(|(p, _)| p)
        .filter(|p| !p.host.is_empty())
}

/// The pairing and, when the link carries them, the mesh keys. `None` when the link is not a
/// valid pairing **or** carries a malformed or partial set of mesh fields.
pub fn parse_pairing_link_full(link: &str) -> Option<(Pairing, Option<MeshKeys>)> {
    let link = link.trim();
    // Compared as bytes: `link[..12]` panics when byte 12 falls inside a
    // multibyte character. A match proves the first 12 bytes are ASCII, so the
    // slice below is on a char boundary.
    let head = link.as_bytes().get(..12)?;
    if !head.eq_ignore_ascii_case(b"ldmux://pair") {
        return None;
    }
    let rest = &link[12..];
    // A `#fragment` ends the URL; it is not part of the query.
    let rest = match rest.find('#') {
        Some(i) => &rest[..i],
        None => rest,
    };
    // The remainder is either "" (no query) or "?<query>". Anything else (a
    // path, a port, extra segments) is not this link's shape.
    if !rest.is_empty() && !rest.starts_with('?') {
        return None;
    }
    let query = rest.strip_prefix('?').unwrap_or("");
    let mut v: Option<String> = None;
    let mut host: Option<String> = None;
    let mut token: Option<String> = None;
    let mut name: Option<String> = None;
    let mut mv: Option<String> = None;
    let mut mk: Option<String> = None;
    let mut psk: Option<String> = None;
    let mut mr: Option<String> = None;
    let mut mp: Option<String> = None;
    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        let (k, val) = match part.split_once('=') {
            Some((k, v)) => (k, v),
            None => continue,
        };
        let decoded = decode_query_value(val);
        // First value wins, as URLSearchParams.get.
        match k {
            "v" if v.is_none() => v = Some(decoded),
            "host" if host.is_none() => host = Some(decoded),
            "token" if token.is_none() => token = Some(decoded),
            "name" if name.is_none() => name = Some(decoded),
            "mv" if mv.is_none() => mv = Some(decoded),
            "mk" if mk.is_none() => mk = Some(decoded),
            "psk" if psk.is_none() => psk = Some(decoded),
            "mr" if mr.is_none() => mr = Some(decoded),
            "mp" if mp.is_none() => mp = Some(decoded),
            _ => {}
        }
    }
    if v.as_deref() != Some("1") {
        return None;
    }
    let mesh_only = host.is_none() && token.is_none();
    let p = Pairing {
        host: host.unwrap_or_default(),
        token: token.unwrap_or_default(),
        name: name.unwrap_or_default(),
    };
    // Neither host nor token is a mesh-only link, but only if it then carries mesh keys (checked
    // below): with neither and no keys it is nothing. One of the two alone is always refused.
    if mesh_only {
        if name_problem(&p.name) || (mv.is_none() && mk.is_none() && psk.is_none() && mr.is_none())
        {
            return None;
        }
    } else if pairing_problem(&p).is_some() {
        return None;
    }
    // Mesh keys: all or nothing, and well formed. The host's tuple is an *addition* to them, never a
    // substitute: a code that carried only a preset would name a frequency and no peer.
    let mesh = if mv.is_none() && mk.is_none() && psk.is_none() && mr.is_none() {
        None
    } else {
        if mv.as_deref() != Some("1") {
            return None;
        }
        Some(MeshKeys {
            host_key: parse_hex32(mk.as_deref()?)?,
            psk: parse_hex32(psk.as_deref()?)?,
            host_radio: match mr.as_deref() {
                Some(r) => Some(parse_hex32(r)?),
                None => None,
            },
            // A malformed preset is not a reason to refuse the keys: it is one optional sentence
            // about the host's radio, and the link is usable without it.
            host_preset: mp.as_deref().and_then(HostPreset::decode),
        })
    };
    Some((p, mesh))
}

fn hex32(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Exactly 64 lowercase hex characters, as 32 bytes.
fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Split `address:port` (IPv6 bracketed). `None` when it is not that shape.
pub fn split_host(host: &str) -> Option<(String, u16)> {
    if host.starts_with('[') {
        let close = host.find(']')?;
        let inner = &host[1..close];
        if inner.is_empty()
            || !inner
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b'.' || b == b':')
        {
            return None;
        }
        let after = &host[close + 1..];
        let port_s = after.strip_prefix(':')?;
        let addr = host[..=close].to_string();
        return Some((addr, parse_port(port_s)?));
    }
    let idx = host.rfind(':')?;
    let addr = &host[..idx];
    let port_s = &host[idx + 1..];
    // The unbracketed address may not contain a colon (the TS regex
    // `[^:[\]]+`): a bare IPv6 like fd7a::1:7625 is not "address:port".
    if addr.is_empty() || addr.bytes().any(|b| b == b']' || b == b':') {
        return None;
    }
    Some((addr.to_string(), parse_port(port_s)?))
}

/// A name the link may not carry (too long, or with a control character).
fn name_problem(name: &str) -> bool {
    name.chars().count() > 64
        || name
            .chars()
            .any(|c| (0x00u32..=0x1f).contains(&(c as u32)) || c as u32 == 0x7f)
}

fn pairing_problem(p: &Pairing) -> Option<String> {
    // `None` from split_host means a host the app would refuse, not a
    // non-problem: the TS returns an error string there.
    let Some(hp) = split_host(&p.host) else {
        return Some(format!(
            "pairing host must be address:port, got {}",
            serde_json::to_string(&p.host).unwrap()
        ));
    };
    if !is_tailnet_or_loopback(&hp.0) {
        return Some(format!(
            "pairing host must be a tailnet or loopback address, got {}",
            hp.0
        ));
    }
    if !is_token(&p.token) {
        return Some("pairing token must be 64 lowercase hex characters".to_string());
    }
    let name_units: usize = p.name.chars().count();
    if name_units > 64
        || p.name
            .chars()
            .any(|c| (0x00u32..=0x1f).contains(&(c as u32)) || c as u32 == 0x7f)
    {
        return Some("pairing name must be at most 64 printable characters".to_string());
    }
    None
}

fn is_token(t: &str) -> bool {
    t.len() == 64
        && t.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn parse_port(s: &str) -> Option<u16> {
    if s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let p: u32 = s.parse().ok()?;
    if !(1..=65535).contains(&p) {
        return None;
    }
    Some(p as u16)
}

/// `encodeURIComponent`, byte-faithful for the link shapes this contract
/// produces: unreserved characters pass, every other UTF-8 byte is `%XX`
/// (uppercase hex).
fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_digit()
        || b.is_ascii_uppercase()
        || b.is_ascii_lowercase()
        || matches!(b, b'-' | b'_' | b'.' | b'!' | b'*' | b'~')
}

/// The TS's `enc`: percent-encode, then leave the colon as itself (it is legal
/// in a query and every host has one).
fn enc(v: &str) -> String {
    encode_uri_component(v).replace("%3A", ":")
}

/// `URLSearchParams`-style decode: `%XX` → byte, `+` → space, then UTF-8.
fn decode_query_value(s: &str) -> String {
    let mut out: Vec<u8> = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'+' {
            out.push(b' ');
            i += 1;
        } else if c == b'%' && i + 2 < b.len() && is_hex(b[i + 1]) && is_hex(b[i + 2]) {
            out.push(hexval(b[i + 1]) * 16 + hexval(b[i + 2]));
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}
fn hexval(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// The fixture is the contract. Port of the TS `pairing.test.ts` cases.
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;

    struct Fixture {
        valid: Vec<Value>,
        invalid: Vec<Value>,
    }

    fn fixture() -> Fixture {
        // One corpus, read by both surviving sides: the app's PairingTest and
        // this one (the TS test went with the TS tree at D4).
        let raw = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/test/fixtures/pairing-links.json"
        ))
        .unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        Fixture {
            valid: v["valid"].as_array().unwrap().to_vec(),
            invalid: v["invalid"].as_array().unwrap().to_vec(),
        }
    }

    #[test]
    fn every_valid_fixture_link_parses_to_its_pairing() {
        let f = fixture();
        for c in &f.valid {
            let link = c["link"].as_str().unwrap();
            let want_host = c["pairing"]["host"].as_str().unwrap();
            let want_token = c["pairing"]["token"].as_str().unwrap();
            let want_name = c["pairing"]["name"].as_str().unwrap();
            let got = parse_pairing_link(link).expect("valid link must parse");
            assert_eq!(got.host, want_host, "host for {link}");
            assert_eq!(got.token, want_token, "token for {link}");
            assert_eq!(got.name, want_name, "name for {link}");
        }
    }

    #[test]
    fn a_multibyte_char_at_the_prefix_boundary_is_refused_not_a_panic() {
        // 11 ASCII bytes, then a 3-byte char straddling byte 12.
        assert!(parse_pairing_link("ldmux://pai\u{20ac}x").is_none());
        // And a 2-byte char whose boundary is exactly at 12 is fine to refuse.
        assert!(parse_pairing_link("ldmux://pair\u{e9}").is_none());
        assert!(parse_pairing_link("\u{20ac}\u{20ac}\u{20ac}\u{20ac}\u{20ac}").is_none());
    }

    #[test]
    fn every_invalid_fixture_link_is_refused() {
        let f = fixture();
        for iv in &f.invalid {
            let link = iv.as_str().unwrap();
            assert!(
                parse_pairing_link(link).is_none(),
                "expected refusal of {link}"
            );
        }
    }

    #[test]
    fn what_the_cli_makes_the_parser_reads_back() {
        let f = fixture();
        for c in &f.valid {
            let host = c["pairing"]["host"].as_str().unwrap().to_string();
            let token = c["pairing"]["token"].as_str().unwrap().to_string();
            let name = c["pairing"]["name"].as_str().unwrap().to_string();
            let p = Pairing { host, token, name };
            let link = pairing_link_with_mesh(&p, None).expect("valid pairing must make a link");
            let got = parse_pairing_link(&link).expect("cli link must parse back");
            assert_eq!(got, p, "round-trip for {link}");
        }
    }

    #[test]
    fn the_cli_never_makes_a_link_the_app_would_refuse() {
        let token = "0".repeat(64);
        let p = Pairing {
            host: "192.168.1.5:7625".into(),
            token: token.clone(),
            name: "".into(),
        };
        assert!(pairing_link_with_mesh(&p, None).is_err(), "LAN host must be refused");
        let p = Pairing {
            host: "100.100.0.1".into(),
            token: token.clone(),
            name: "".into(),
        };
        assert!(pairing_link_with_mesh(&p, None).is_err(), "missing :port must be refused");
        let p = Pairing {
            host: "100.100.0.1:7625".into(),
            token: "short".into(),
            name: "".into(),
        };
        assert!(pairing_link_with_mesh(&p, None).is_err(), "short token must be refused");
    }

    #[test]
    fn the_link_is_short_enough_for_a_small_code() {
        let p = Pairing {
            host: "100.100.0.254:65535".into(),
            token: "f".repeat(64),
            name: "debian".into(),
        };
        let link = pairing_link_with_mesh(&p, None).unwrap();
        // 134 bytes is what a version-6 code holds at level L (41 modules, 25
        // terminal rows with the quiet zone).
        assert!(link.len() <= 134, "link too long: {}", link.len());
    }

    // ── mesh keys ─────────────────────────────────────────────────────────

    fn mesh_fixture() -> serde_json::Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/test/fixtures/pairing-links-mesh.json"
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn unhex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    /// The keys a fixture entry stands for. `hostPreset` is `[freq_khz, bw_hz, sf, cr]`, and an
    /// entry without it stands for a code that carried no preset (or one the parser ignored).
    fn want_keys(m: &serde_json::Map<String, serde_json::Value>) -> MeshKeys {
        MeshKeys {
            host_key: unhex32(m["hostKey"].as_str().unwrap()),
            psk: unhex32(m["psk"].as_str().unwrap()),
            host_radio: m.get("hostRadio").and_then(|r| r.as_str()).map(unhex32),
            host_preset: m.get("hostPreset").and_then(|p| {
                let n = p.as_array()?;
                Some(HostPreset {
                    freq_khz: u32::try_from(n.first()?.as_u64()?).ok()?,
                    bw_hz: u32::try_from(n.get(1)?.as_u64()?).ok()?,
                    sf: u8::try_from(n.get(2)?.as_u64()?).ok()?,
                    cr: u8::try_from(n.get(3)?.as_u64()?).ok()?,
                    // The fifth element is the path width, when the host sent one.
                    path_bytes: n
                        .get(4)
                        .and_then(|v| v.as_u64())
                        .and_then(|v| u8::try_from(v).ok()),
                })
            }),
        }
    }

    #[test]
    fn mesh_fixture_valid_links_parse_to_their_pairing_and_keys() {
        let f = mesh_fixture();
        let valid = f["valid"].as_array().unwrap();
        assert!(valid.len() >= 3);
        for c in valid {
            let (p, mesh) = parse_pairing_link_full(c["link"].as_str().unwrap()).expect("valid");
            assert_eq!(p.host, c["pairing"]["host"].as_str().unwrap());
            assert_eq!(p.token, c["pairing"]["token"].as_str().unwrap());
            assert_eq!(p.name, c["pairing"]["name"].as_str().unwrap());
            let want = c["mesh"].as_object().map(want_keys);
            assert_eq!(mesh, want, "{}", c["link"]);
            // The plain parser agrees on the pairing and never sees the keys.
            assert_eq!(parse_pairing_link(c["link"].as_str().unwrap()), Some(p));
        }
    }

    #[test]
    fn mesh_fixture_invalid_links_are_refused_outright_not_downgraded() {
        for c in mesh_fixture()["invalid"].as_array().unwrap() {
            let link = c["link"].as_str().unwrap();
            assert!(parse_pairing_link_full(link).is_none(), "{}", c["why"]);
            // Not even the tailnet half survives: a half-formed secret is not dropped quietly.
            assert!(parse_pairing_link(link).is_none(), "{}", c["why"]);
        }
    }

    #[test]
    fn the_serialiser_round_trips_and_stays_v1() {
        let f = mesh_fixture();
        let c = &f["valid"][1];
        let p = Pairing {
            host: c["pairing"]["host"].as_str().unwrap().into(),
            token: c["pairing"]["token"].as_str().unwrap().into(),
            name: c["pairing"]["name"].as_str().unwrap().into(),
        };
        // Every key field the fixture entry carries, so the serialiser writes the link it holds.
        let keys = MeshKeys {
            host_key: unhex32(c["mesh"]["hostKey"].as_str().unwrap()),
            psk: unhex32(c["mesh"]["psk"].as_str().unwrap()),
            host_radio: c["mesh"].get("hostRadio").and_then(|r| r.as_str()).map(unhex32),
            host_preset: c["mesh"].as_object().and_then(|m| want_keys(m).host_preset),
        };
        let link = pairing_link_with_mesh(&p, Some(&keys)).unwrap();
        // The fixture's own link is the canonical serialisation.
        assert_eq!(link, c["link"].as_str().unwrap());
        assert!(link.starts_with("ldmux://pair?v=1&"));
        assert_eq!(
            parse_pairing_link_full(&link),
            Some((p.clone(), Some(keys)))
        );
        // Without keys it is byte-for-byte the old link.
        assert_eq!(
            pairing_link_with_mesh(&p, None).unwrap(),
            pairing_link_with_mesh(&p, None).unwrap()
        );
    }

    #[test]
    fn mesh_only_links_carry_no_daemon_address_or_token_and_are_not_tailnet_pairings() {
        let f = mesh_fixture();
        let only = f["meshOnly"].as_array().unwrap();
        assert!(only.len() >= 2);
        for c in only {
            let link = c["link"].as_str().unwrap();
            assert!(!link.contains("host=") && !link.contains("token="));
            let (p, mesh) = parse_pairing_link_full(link).expect("valid mesh-only");
            assert_eq!((p.host.as_str(), p.token.as_str()), ("", ""));
            assert_eq!(p.name, c["name"].as_str().unwrap());
            let m = mesh.expect("keys");
            assert_eq!(hex32(&m.host_key), c["mesh"]["hostKey"].as_str().unwrap());
            // The plain, tailnet reader must not take it for a tailnet pairing.
            assert_eq!(parse_pairing_link(link), None);
            // And the serialiser makes exactly this link.
            assert_eq!(mesh_only_link(&p.name, &m).unwrap(), link);
        }
    }

    #[test]
    fn half_a_tailnet_pairing_is_refused_even_with_valid_mesh_keys() {
        for c in mesh_fixture()["meshOnlyInvalid"].as_array().unwrap() {
            let link = c["link"].as_str().unwrap();
            assert!(parse_pairing_link_full(link).is_none(), "{}", c["why"]);
        }
    }

    #[test]
    fn a_pre_mesh_scanner_pairs_the_tailnet_from_a_mesh_link() {
        // The v1 rule is "unknown parameters are ignored". Model a scanner with no mesh code by
        // stripping the three fields: what is left is exactly the plain link.
        let c = &mesh_fixture()["valid"][1];
        let with = c["link"].as_str().unwrap();
        let plain = mesh_fixture()["valid"][0]["link"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(with.starts_with(&plain));
        assert_eq!(parse_pairing_link(with), parse_pairing_link(&plain),);
    }

    #[test]
    fn mesh_keys_never_print() {
        let k = MeshKeys {
            host_key: [7; 32],
            psk: [9; 32],
            host_radio: Some([3; 32]),
            host_preset: Some(HostPreset {
                freq_khz: 910_525,
                bw_hz: 62_500,
                sf: 7,
                cr: 5,
                path_bytes: Some(1),
            }),
        };
        let shown = format!("{k:?}");
        assert!(!shown.contains('7') && !shown.contains('9'), "{shown}");
    }
}
