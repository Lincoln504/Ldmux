//! Which addresses ldmux may listen on and accept connections from.
//!
//! "Tailnet only" (AGENTS.md) is a rule about addresses, so it is written down
//! as one: loopback, or an address Tailscale assigns — `100.64.0.0/10` (the
//! CGNAT range Tailscale allocates IPv4 from) and `fd7a:115c:a1e0::/48` (its
//! IPv6 prefix). Anything else is a LAN or the internet, and the daemon
//! neither binds it nor answers it.
//!
//! Literal addresses only. A name would have to be resolved to be checked,
//! and a name that resolves differently tomorrow is not a rule; the deployment
//! names the tailnet address itself (`LDMUX_REMOTE_HOST`). `localhost` is the
//! one name accepted, because it is loopback by definition.
//!
//! Port of `packages/protocol/src/net.ts` (the TS original; deleted at D4, readable at git commit `6b3efb8`). The Node
//! `BlockList` is replaced by direct range tests; the classification table is
//! the same.

/// The three classes a socket peer can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressKind {
    Loopback,
    Tailnet,
    Other,
}

/// Classify one address as a socket reports it.
///
/// An IPv4 address may arrive IPv6-mapped (`::ffff:100.64.1.2`) from a
/// dual-stack socket; it is judged as the IPv4 address it is. IPv6 zone ids
/// (`fe80::1%eth0`) are never tailnet or loopback, and are rejected as
/// `Other`.
pub fn classify_address(raw: &str) -> AddressKind {
    let mut addr = raw.trim();
    if addr == "localhost" {
        return AddressKind::Loopback;
    }
    if addr.starts_with('[') && addr.ends_with(']') {
        addr = &addr[1..addr.len() - 1];
    }
    // IPv6-mapped form: judge as the embedded IPv4.
    if let Some(stripped) = addr.strip_prefix("::ffff:") {
        if let Some(v4) = parse_ipv4(stripped) {
            return classify_v4(v4);
        }
        return AddressKind::Other;
    }
    if let Some(v4) = parse_ipv4(addr) {
        return classify_v4(v4);
    }
    if let Some(v6) = parse_ipv6(addr) {
        return classify_v6(&v6);
    }
    AddressKind::Other
}

/// May the daemon listen here, and may a peer at this address talk to it?
pub fn is_tailnet_or_loopback(addr: &str) -> bool {
    classify_address(addr) != AddressKind::Other
}

fn classify_v4(a: [u8; 4]) -> AddressKind {
    if a[0] == 127 {
        return AddressKind::Loopback;
    }
    // 100.64.0.0/10: first octet 100, second octet in 64..=127.
    if a[0] == 100 && (64..=127).contains(&a[1]) {
        return AddressKind::Tailnet;
    }
    AddressKind::Other
}

fn classify_v6(g: &[u16; 8]) -> AddressKind {
    // loopback ::1
    let is_loopback = g[0..7] == [0; 7] && g[7] == 1;
    if is_loopback {
        return AddressKind::Loopback;
    }
    // fd7a:115c:a1e0::/48
    if g[0] == 0xfd7a && g[1] == 0x115c && g[2] == 0xa1e0 {
        return AddressKind::Tailnet;
    }
    AddressKind::Other
}

fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut out = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 1 && p.starts_with('0') {
            return None;
        }
        let n: u32 = p.parse().ok()?;
        if n > 255 {
            return None;
        }
        out[i] = n as u8;
    }
    Some(out)
}

fn parse_ipv6(s: &str) -> Option<[u16; 8]> {
    if s.contains('%') {
        return None; // zone id: never loopback or tailnet
    }
    if s.matches("::").count() > 1 {
        return None;
    }
    let (lstr, rstr, has_compression) = match s.find("::") {
        Some(pos) => (&s[..pos], &s[pos + 2..], true),
        None => (s, "", false),
    };
    let left: Vec<u16> = if lstr.is_empty() {
        vec![]
    } else {
        let v: Option<Vec<u16>> = lstr
            .split(':')
            .map(|g| u16::from_str_radix(g, 16).ok())
            .collect();
        v?
    };
    let right: Vec<u16> = if rstr.is_empty() {
        vec![]
    } else {
        let v: Option<Vec<u16>> = rstr
            .split(':')
            .map(|g| u16::from_str_radix(g, 16).ok())
            .collect();
        v?
    };
    let total = left.len() + right.len();
    if !has_compression {
        if total != 8 {
            return None;
        }
    } else if total >= 8 {
        return None;
    }
    let mut out = [0u16; 8];
    for (i, g) in left.iter().enumerate() {
        out[i] = *g;
    }
    let rstart = 8 - right.len();
    for (i, g) in right.iter().enumerate() {
        out[rstart + i] = *g;
    }
    Some(out)
}

/// The TS test's classification table, ported 1:1. The same addresses, the
/// same expected classes — so a regression in either tree is caught.
#[cfg(test)]
mod tests {
    use super::*;

    fn expect_kind(cases: &[(&str, AddressKind)]) {
        for (a, want) in cases {
            assert_eq!(classify_address(a), *want, "classify_address({a:?})");
        }
    }

    #[test]
    fn loopback_in_every_form_a_socket_reports_it() {
        expect_kind(&[
            ("127.0.0.1", AddressKind::Loopback),
            ("127.255.255.254", AddressKind::Loopback),
            ("::1", AddressKind::Loopback),
            ("[::1]", AddressKind::Loopback),
            ("::ffff:127.0.0.1", AddressKind::Loopback),
            ("localhost", AddressKind::Loopback),
        ]);
    }

    #[test]
    fn the_tailnet_ranges_edges_included() {
        expect_kind(&[
            ("100.64.0.0", AddressKind::Tailnet),
            ("100.127.119.54", AddressKind::Tailnet),
            ("100.127.255.255", AddressKind::Tailnet),
            ("::ffff:100.100.1.2", AddressKind::Tailnet),
            ("fd7a:115c:a1e0::1", AddressKind::Tailnet),
            ("fd7a:115c:a1e0:ffff::1", AddressKind::Tailnet),
        ]);
    }

    #[test]
    fn one_past_either_end_lan_internet_and_names_are_not_allowed() {
        expect_kind(&[
            ("100.63.255.255", AddressKind::Other),
            ("100.128.0.0", AddressKind::Other),
            ("0.0.0.0", AddressKind::Other),
            ("::", AddressKind::Other),
            ("192.168.1.5", AddressKind::Other),
            ("10.0.0.1", AddressKind::Other),
            ("8.8.8.8", AddressKind::Other),
            ("fd7a:115c:a1e1::1", AddressKind::Other),
            ("fe80::1%eth0", AddressKind::Other),
            ("debian.tail496756.ts.net", AddressKind::Other),
            ("", AddressKind::Other),
            ("not an address", AddressKind::Other),
        ]);
    }

    #[test]
    fn is_tailnet_or_loopback_matches_classification() {
        assert!(is_tailnet_or_loopback("127.0.0.1"));
        assert!(is_tailnet_or_loopback("100.127.119.54"));
        assert!(!is_tailnet_or_loopback("8.8.8.8"));
        assert!(!is_tailnet_or_loopback("localhost-lookalike"));
    }
}
