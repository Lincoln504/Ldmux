//! What `ldmux code` and `ldmux pair` show: the pairing for the address this
//! daemon actually listens on (ADR-0016, ADR-0017).
//!
//! Port of `packages/daemon/src/pairing.ts` (the TS original; deleted at D4, readable at git commit `6b3efb8`).
//!
//! Built from the listen info, never from a guess about how the machine is
//! exposed. The previous version assembled a `tailscale serve` URL and a deep
//! link with port 443 for an architecture that was not deployed, so the one
//! output meant to connect a phone pointed it at a port where nothing
//! listened.

use ldmux_protocol::{
    net::AddressKind,
    pairing::{MeshKeys, Pairing},
};

/// The listen facts the pairing is built from.
pub struct ListenInfo {
    /// The address the WebSocket is bound to, as a string ("100.100.0.1",
    /// "127.0.0.1", "fd7a::1", …).
    pub hostname: String,
    pub port: u16,
    pub token: String,
    /// Whether the WebSocket is up at all.
    pub listening: bool,
}

/// What is wrong, and the message the CLI shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingProblem {
    /// The WebSocket is not up yet.
    NotUpYet,
    /// The daemon listens on loopback only, which a phone cannot reach.
    LoopbackOnly,
    /// The constructed pairing is one the app would refuse. Unreachable for a
    /// daemon minted by `auth` (64-hex token, sanitized name) unless the
    /// listen address is neither tailnet nor loopback — the TS source threw
    /// the same refusal out of `pairingLink`.
    LinkRejected(String),
}

impl PairingProblem {
    /// The exact message the TS source threw.
    pub fn message(&self, hostname: &str) -> String {
        match self {
            PairingProblem::NotUpYet => "the daemon's WebSocket is not up yet (is tailscale up?); \
                                         try again in a few seconds"
                .to_string(),
            PairingProblem::LoopbackOnly => format!(
                "the daemon listens on {hostname} only, which a phone cannot reach: \
                 set LDMUX_REMOTE_HOST to this machine's tailnet address (`tailscale ip -4`) \
                 in deploy/ldmux.service, then re-run the install (`nix run .#install`)",
            ),
            PairingProblem::LinkRejected(why) => why.clone(),
        }
    }
}

/// What `ldmux code` prints: the pairing as a QR code a phone can scan, then
/// the machine's name and the address, so a human can check it is the right box.
///
/// Here rather than in the CLI so the rendering is testable without a daemon:
/// the CLI's whole job is to print this (the TS `encodeQr` +
/// `renderQrTerminal`).
pub fn code_text(info: &ListenInfo, name: &str) -> Result<String, PairingProblem> {
    code_text_with_mesh(info, name, None)
}

/// As [`code_text`], with the mesh keys in the link when given.
pub fn code_text_with_mesh(
    info: &ListenInfo,
    name: &str,
    mesh: Option<&MeshKeys>,
) -> Result<String, PairingProblem> {
    let (pairing, link) = pairing_for_with_mesh(info, name, mesh)?;
    let code =
        crate::qr::encode_qr(&link, crate::qr::Ecc::L).map_err(PairingProblem::LinkRejected)?;
    Ok(format!(
        "{}{}  {}\n",
        crate::qr::render_qr_terminal(&code, 1),
        pairing.name,
        pairing.host
    ))
}

/// The mesh-only link and its QR: no tailnet address and no daemon bearer token, so it needs no
/// listening daemon and hands a radio-only phone nothing it cannot use.
pub fn mesh_only_code_text(name: &str, mesh: &MeshKeys) -> Result<String, PairingProblem> {
    let link = ldmux_protocol::pairing::mesh_only_link(name, mesh)
        .map_err(|e| PairingProblem::LinkRejected(e.0))?;
    let code =
        crate::qr::encode_qr(&link, crate::qr::Ecc::L).map_err(PairingProblem::LinkRejected)?;
    Ok(format!(
        "{}{}  (mesh only)\n",
        crate::qr::render_qr_terminal(&code, 1),
        name
    ))
}

/// The pairing a phone should use, or a problem saying what to fix.
pub fn pairing_for(info: &ListenInfo, name: &str) -> Result<(Pairing, String), PairingProblem> {
    pairing_for_with_mesh(info, name, None)
}

/// As [`pairing_for`], with the mesh keys appended to the link when given.
pub fn pairing_for_with_mesh(
    info: &ListenInfo,
    name: &str,
    mesh: Option<&MeshKeys>,
) -> Result<(Pairing, String), PairingProblem> {
    if !info.listening {
        return Err(PairingProblem::NotUpYet);
    }
    if ldmux_protocol::net::classify_address(&info.hostname) == AddressKind::Loopback {
        return Err(PairingProblem::LoopbackOnly);
    }
    // What the app calls the machine. Trimmed to what the link allows rather
    // than refused: a long hostname is not a reason not to pair. (Control
    // characters U+0000–U+001F and U+007F come out; then at most 64 characters —
    // the TS slices UTF-16 units, which for the inputs that matter are the
    // same count as Unicode scalars.)
    let name: String = name
        .chars()
        .filter(|c| {
            let v = *c as u32;
            !(v <= 0x1f || v == 0x7f)
        })
        .take(64)
        .collect();
    // Bracket IPv6 so `host:port` parses as one address, not an address plus
    // a port.
    let address = if info.hostname.contains(':') {
        format!("[{}]", info.hostname)
    } else {
        info.hostname.clone()
    };
    let pairing = Pairing {
        host: format!("{}:{}", address, info.port),
        token: info.token.clone(),
        name,
    };
    let link = ldmux_protocol::pairing::pairing_link_with_mesh(&pairing, mesh)
        .map_err(|e| PairingProblem::LinkRejected(e.0))?;
    Ok((pairing, link))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_protocol::pairing::parse_pairing_link;

    fn token() -> String {
        "0123456789abcdef".repeat(4)
    }

    fn live(hostname: &str, port: u16) -> ListenInfo {
        ListenInfo {
            hostname: hostname.to_string(),
            port,
            token: token(),
            listening: true,
        }
    }

    #[test]
    fn the_pairing_is_the_address_the_daemon_listens_on_not_a_guess() {
        let (pairing, link) = pairing_for(&live("100.100.0.1", 7625), "debian").unwrap();
        assert_eq!(
            &pairing,
            &Pairing {
                host: "100.100.0.1:7625".to_string(),
                token: token(),
                name: "debian".to_string(),
            }
        );
        let parsed = parse_pairing_link(&link).expect("link must parse");
        assert_eq!(&pairing, &parsed);
    }

    #[test]
    fn an_ipv6_tailnet_address_is_bracketed() {
        let (pairing, _) = pairing_for(&live("fd7a:115c:a1e0::5", 7625), "d").unwrap();
        assert_eq!(pairing.host, "[fd7a:115c:a1e0::5]:7625");
    }

    #[test]
    fn a_daemon_on_loopback_cannot_be_paired_with_and_says_what_to_set() {
        let err = pairing_for(&live("127.0.0.1", 7625), "d").unwrap_err();
        match err {
            PairingProblem::LoopbackOnly => {}
            other => panic!("expected LoopbackOnly, got {other:?}"),
        }
        let msg = PairingProblem::LoopbackOnly.message("127.0.0.1");
        assert!(msg.contains("LDMUX_REMOTE_HOST"));
    }

    #[test]
    fn a_daemon_whose_websocket_is_not_up_yet_says_so() {
        let info = ListenInfo {
            hostname: "127.0.0.1".to_string(),
            port: 0,
            token: token(),
            listening: false,
        };
        let err = pairing_for(&info, "d").unwrap_err();
        match err {
            PairingProblem::NotUpYet => {}
            other => panic!("expected NotUpYet, got {other:?}"),
        }
        assert!(PairingProblem::NotUpYet.message("").contains("not up yet"));
    }

    #[test]
    fn a_long_or_odd_hostname_is_trimmed_to_what_the_link_allows() {
        let name = "x".repeat(80) + "\n";
        let (pairing, _) = pairing_for(&live("100.100.0.1", 7625), &name).unwrap();
        assert_eq!(pairing.name, "x".repeat(64));
    }

    #[test]
    fn the_deployed_shape_of_link_fits_a_version_6_code() {
        // The widest IPv4 tailnet address, the widest port, and a typical
        // name.
        let (_, link) = pairing_for(&live("100.127.255.254", 65535), "debian").unwrap();
        let code = crate::qr::encode_qr(&link, crate::qr::Ecc::L).unwrap();
        assert!(code.version <= 6);
    }

    /// `ldmux code` is the QR, not the link: a phone scans it. It printed the
    /// bare link (identical to `ldmux pair`) after the port, which left the
    /// documented way to pair a phone with nothing to scan.
    #[test]
    fn code_text_is_a_qr_block_and_what_it_is_for() {
        let text = code_text(&live("100.100.0.1", 7625), "debian").unwrap();
        // A QR drawn with the half-block character and 256-colour escapes, not a
        // line of text: many rows of it, and the link itself absent.
        assert!(text.contains('\u{2580}'), "no QR block in: {text}");
        assert!(
            text.contains("\u{1b}[38;5;16;48;5;231m"),
            "no QR colours in: {text}"
        );
        assert!(text.lines().count() > 5, "too few rows to be a QR: {text}");
        assert!(text.contains("debian  100.100.0.1:7625"), "{text}");
        assert!(!text.contains("ldmux://"), "code printed the link: {text}");
    }

    #[test]
    fn code_text_refuses_a_daemon_a_phone_cannot_reach() {
        match code_text(&live("127.0.0.1", 7625), "d") {
            Err(PairingProblem::LoopbackOnly) => {}
            other => panic!("expected LoopbackOnly, got {other:?}"),
        }
    }

    #[test]
    fn a_mesh_link_carries_the_keys_stays_v1_and_still_fits_a_code() {
        use ldmux_protocol::pairing::{MeshKeys, parse_pairing_link_full};
        let keys = MeshKeys {
            host_key: [0x11; 32],
            psk: [0x22; 32],
            host_radio: Some([0x33; 32]),
            // The host's tuple rides along, and the link still fits a code.
            host_preset: Some(ldmux_protocol::pairing::HostPreset {
                freq_khz: 910_525,
                bw_hz: 500_000,
                sf: 7,
                cr: 5,
                path_bytes: Some(2),
            }),
        };
        let (pairing, link) =
            pairing_for_with_mesh(&live("100.100.0.1", 7625), "debian", Some(&keys)).unwrap();
        assert!(link.starts_with("ldmux://pair?v=1&"), "{link}");
        assert_eq!(
            parse_pairing_link_full(&link),
            Some((pairing, Some(keys.clone())))
        );
        // Without keys it is the old link, byte for byte.
        let (_, plain) = pairing_for(&live("100.100.0.1", 7625), "debian").unwrap();
        assert!(link.starts_with(&plain));
        assert!(!plain.contains("psk="));
        // The longer link still encodes as a QR code.
        let text = code_text_with_mesh(&live("100.100.0.1", 7625), "debian", Some(&keys)).unwrap();
        assert!(text.contains("debian"));
        assert!(
            !text.contains(&"22".repeat(32)),
            "the code text must not print the secret as text"
        );
    }

    #[test]
    fn a_mesh_only_code_needs_no_listening_daemon_and_names_no_address() {
        let keys = MeshKeys {
            host_key: [7; 32],
            psk: [9; 32],
            host_radio: None,
            host_preset: None,
        };
        let text = mesh_only_code_text("debian", &keys).expect("renders");
        assert!(text.trim_end().ends_with("debian  (mesh only)"), "{text}");
        let link = ldmux_protocol::pairing::mesh_only_link("debian", &keys).unwrap();
        assert!(!link.contains("host=") && !link.contains("token="));
        // The tailnet reader refuses it; the full reader takes it as keys and a name only.
        assert_eq!(ldmux_protocol::pairing::parse_pairing_link(&link), None);
        let (p, m) = ldmux_protocol::pairing::parse_pairing_link_full(&link).unwrap();
        assert!(p.host.is_empty() && p.token.is_empty() && m == Some(keys));
    }
}
