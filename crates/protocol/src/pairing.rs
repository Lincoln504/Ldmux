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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingError(pub String);

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for PairingError {}

/// The link for `p`. Errors on a pairing the app would refuse, so the CLI
/// never shows one.
pub fn pairing_link(p: &Pairing) -> Result<String, PairingError> {
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
    let q: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
    Ok(format!("ldmux://pair?{}", q.join("&")))
}

/// The pairing in `link`, or `None` if it is not a valid ldmux pairing link.
///
/// Scheme and host are case-insensitive (a URL normalises them); the query is
/// case-sensitive and a `#fragment` is dropped. A repeated parameter reads as
/// its first value (as `URLSearchParams.get` does).
pub fn parse_pairing_link(link: &str) -> Option<Pairing> {
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
            _ => {}
        }
    }
    if v.as_deref() != Some("1") {
        return None;
    }
    let p = Pairing {
        host: host.unwrap_or_default(),
        token: token.unwrap_or_default(),
        name: name.unwrap_or_default(),
    };
    if pairing_problem(&p).is_some() {
        return None;
    }
    Some(p)
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
            let link = pairing_link(&p).expect("valid pairing must make a link");
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
        assert!(pairing_link(&p).is_err(), "LAN host must be refused");
        let p = Pairing {
            host: "100.100.0.1".into(),
            token: token.clone(),
            name: "".into(),
        };
        assert!(pairing_link(&p).is_err(), "missing :port must be refused");
        let p = Pairing {
            host: "100.100.0.1:7625".into(),
            token: "short".into(),
            name: "".into(),
        };
        assert!(pairing_link(&p).is_err(), "short token must be refused");
    }

    #[test]
    fn the_link_is_short_enough_for_a_small_code() {
        let p = Pairing {
            host: "100.100.0.254:65535".into(),
            token: "f".repeat(64),
            name: "debian".into(),
        };
        let link = pairing_link(&p).unwrap();
        // 134 bytes is what a version-6 code holds at level L (41 modules, 25
        // terminal rows with the quiet zone).
        assert!(link.len() <= 134, "link too long: {}", link.len());
    }
}
