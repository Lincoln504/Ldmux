//! The mesh route's identity files and its status shape.
//!
//! These are the parts of the mesh route that outlive a connection: the static key a phone pinned,
//! the pinned phone key with its replay floor, and the pairing's secret. They used to live beside
//! `SessionHost` in `mesh_session.rs`; the session itself is now `RadioEndpoint`
//! (`docs/RADIO-PLAN.md` §1), and what is left here is the files plus the status the daemon
//! publishes about the link.
//!
//! Persistence is two files in the state directory, both `0600`:
//!
//! - `mesh-session.key`: the host's static key, 64 bytes (private then public), made on first use.
//!   `snow` cannot derive a public key from a private one, so both are stored.
//! - `mesh-pairing.json`: the pinned phone key and the last accepted handshake timestamp. If this
//!   cannot be written the handshake is **refused**: an unrecorded timestamp would reopen replay of
//!   message 1.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use ldmux_session::handshake::{Keypair, PairingEntry};

/// Records an accepted handshake before it is answered.
pub type Persister = Box<dyn FnMut(&PairingEntry) -> std::io::Result<()> + Send>;

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SessionSnapshot {
    /// `off` (no PSK configured), `waiting` (no handshake on this link yet), `established`.
    /// A session that dies (retries exhausted, peer close) goes back to `waiting` with the
    /// reason in `last_error`.
    pub state: &'static str,
    /// The gateway's static public key, hex: what a phone must be given to pair.
    pub public_key: String,
    /// The pinned phone key, hex, once one has handshaken.
    pub peer_key: Option<String>,
    pub handshakes: u32,
    pub handshakes_refused: u32,
    pub sent: u32,
    pub retransmits: u32,
    /// Of those retransmissions, the ones the peer's own ack pulled early rather than a timer.
    pub fast_retransmits: u32,
    pub received_bytes: u64,
    pub dropped: u32,
    pub pending: usize,
    pub budget_stalls: u32,
    pub last_error: Option<String>,
}

impl SessionSnapshot {
    #[must_use]
    pub fn off() -> Self {
        Self {
            state: "off",
            ..Self::default()
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state,
            "publicKey": self.public_key,
            "peerKey": self.peer_key,
            "handshakes": self.handshakes,
            "handshakesRefused": self.handshakes_refused,
            "sent": self.sent,
            "retransmits": self.retransmits,
            "fastRetransmits": self.fast_retransmits,
            "receivedBytes": self.received_bytes,
            "dropped": self.dropped,
            "pending": self.pending,
            "budgetStalls": self.budget_stalls,
            "lastError": self.last_error,
        })
    }
}

/// Both files, loaded or created.
pub struct SessionFiles {
    pub key: Keypair,
    pub pairing: PairingEntry,
    pairing_path: PathBuf,
}

impl SessionFiles {
    /// The pairing as last written to disk. Each L2 connection starts from this, not from the
    /// copy loaded at startup: the handshake timestamp an earlier connection accepted must
    /// still bar a replay of that same message 1 after a reconnect.
    /// Clear the pinned phone key while keeping the replay timestamp. A new pairing grant must
    /// let a re-enrolled phone present a new static key.
    pub fn clear_pin(&self) -> std::io::Result<()> {
        let mut pairing = self.current_pairing();
        pairing.pinned = None;
        write_pairing(&self.pairing_path, &pairing)
    }

    pub fn current_pairing(&self) -> PairingEntry {
        match fs::read_to_string(&self.pairing_path)
            .ok()
            .as_deref()
            .and_then(parse_pairing)
        {
            Some((pinned, last)) => PairingEntry {
                psk: self.pairing.psk,
                pinned,
                last_timestamp_ms: last,
            },
            None => self.pairing.clone(),
        }
    }
}

/// The files the mesh route cannot lose: the static key the phone pinned, the pinned phone
/// key with its replay floor, and the pairing's secret.
pub const PERSISTED_FILES: [&str; 3] = ["mesh-session.key", "mesh-pairing.json", "mesh-psk"];

/// Move those three from `legacy` (a directory that does not survive a reboot) to
/// `persist`, and return the names moved.
///
/// **Why this exists.** Under systemd the state directory is `%t/ldmux`, a tmpfs, so it is
/// empty on every boot. The bearer token was moved out of it for that reason
/// (`deploy/ldmux.service`); the mesh static key was left behind. After a reboot the host
/// generated a new key, the phone's pinned copy no longer matched, `read_message` failed as
/// `Crypto`, no second handshake message was sent, the phone burned its three attempts and
/// landed on `SessionFailed` — and nothing anywhere said the machine had simply rebooted.
///
/// Best effort and idempotent: a file already at the destination wins and the legacy copy is
/// left in place (a copy that reappeared is worth seeing, not deleting). A rename across
/// filesystems is refused by the kernel with `EXDEV`, so the fallback copies, fsyncs and
/// unlinks through the same 0600 writer the file was made with.
pub fn adopt_legacy(legacy: &Path, persist: &Path) -> Vec<String> {
    let mut moved = Vec::new();
    if legacy == persist {
        return moved;
    }
    for name in PERSISTED_FILES {
        let from = legacy.join(name);
        let to = persist.join(name);
        if to.exists() || !from.exists() {
            continue;
        }
        if std::fs::rename(&from, &to).is_ok() {
            moved.push(name.to_string());
            continue;
        }
        // Different filesystems, or the rename was refused: copy through the private writer
        // and only then drop the old copy.
        let Ok(bytes) = std::fs::read(&from) else {
            continue;
        };
        if write_private(&to, &bytes).is_err() {
            continue;
        }
        let _ = std::fs::remove_file(&from);
        moved.push(name.to_string());
    }
    moved
}

impl SessionFiles {
    /// `psk` comes from the environment; everything else lives in `persist_dir`, which must
    /// survive a reboot (see [`adopt_legacy`] for why).
    pub fn load(state_dir: &Path, psk: [u8; 32]) -> Result<Self, String> {
        let key_path = state_dir.join("mesh-session.key");
        let key = match fs::read(&key_path) {
            Ok(b) if b.len() == 64 => {
                let mut private = [0u8; 32];
                let mut public = [0u8; 32];
                private.copy_from_slice(&b[..32]);
                public.copy_from_slice(&b[32..]);
                Keypair::from_parts(private, public)
            }
            Ok(_) => return Err(format!("{} is not a 64-byte key file", key_path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = Keypair::generate().map_err(|e| format!("{e:?}"))?;
                let mut bytes = Vec::with_capacity(64);
                bytes.extend_from_slice(&key.private);
                bytes.extend_from_slice(&key.public);
                write_private(&key_path, &bytes).map_err(|e| e.to_string())?;
                key
            }
            Err(e) => return Err(format!("{}: {e}", key_path.display())),
        };
        let pairing_path = state_dir.join("mesh-pairing.json");
        let (pinned, last) = match fs::read_to_string(&pairing_path) {
            Ok(text) => parse_pairing(&text)
                .ok_or_else(|| format!("{} is unreadable", pairing_path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, 0),
            Err(e) => return Err(format!("{}: {e}", pairing_path.display())),
        };
        Ok(Self {
            key,
            pairing: PairingEntry {
                psk,
                pinned,
                last_timestamp_ms: last,
            },
            pairing_path,
        })
    }

    /// A copy of the static key (each L2 connection builds its own host).
    #[must_use]
    pub fn clone_key(&self) -> Keypair {
        Keypair::from_parts(self.key.private, self.key.public)
    }

    /// The closure the host calls to record an accepted handshake.
    pub fn persister(&self) -> Persister {
        let path = self.pairing_path.clone();
        Box::new(move |p: &PairingEntry| write_pairing(&path, p))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_pairing(path: &Path, pairing: &PairingEntry) -> std::io::Result<()> {
    let json = serde_json::json!({
        "pinned": pairing.pinned.map(|k| hex(&k)),
        "lastTimestampMs": pairing.last_timestamp_ms,
    });
    write_private(path, json.to_string().as_bytes())
}

fn parse_pairing(text: &str) -> Option<(Option<[u8; 32]>, u64)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let last = v.get("lastTimestampMs")?.as_u64()?;
    let pinned = match v.get("pinned")? {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) if s.len() == 64 => {
            let mut k = [0u8; 32];
            for (i, b) in k.iter_mut().enumerate() {
                *b = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
            }
            Some(k)
        }
        _ => return None,
    };
    Some((pinned, last))
}

/// Write a secret file `0600` from the first byte, atomically (write a sibling, then rename).
/// Write a file that only this uid may read, so a crash mid-write cannot leave half of it.
///
/// **The bytes reach the disk before the name does, and the directory is synced after the rename.**
/// The rename is atomic against a *reader*, not against a power cut: without the file's own `fsync`
/// the directory entry can land while the data is still in the page cache, and ext4's documented
/// outcome for that pair is a zero-length file. Without the directory's `fsync` the rename itself may
/// not survive, which for `mesh-pairing.json` means losing the pinned phone key and the replay floor.
/// Both of those are exactly what these files exist to prevent, so this is the one place that writes
/// them.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const PSK: [u8; 32] = [5; 32];

    #[test]
    fn a_wiped_runtime_dir_leaves_the_identity_and_the_psk_alone() {
        // The bug this pins: under systemd the state dir is `%t/ldmux`, a tmpfs, so every boot
        // emptied it, the static key was re-minted, the phone's pinned host key stopped matching
        // and the radio route died with no message. The credential files now live in the persist
        // dir; a runtime dir that vanished must change nothing.
        let root = std::env::temp_dir().join(format!("ldmux-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let persist = root.join("persist");
        let runtime = root.join("run");
        std::fs::create_dir_all(&persist).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();

        let first = SessionFiles::load(&persist, PSK).unwrap();
        let public = first.key.public;
        assert!(persist.join("mesh-session.key").exists());
        // The runtime dir is emptied, as a reboot does, and the daemon starts again.
        std::fs::remove_dir_all(&runtime).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let moved = adopt_legacy(&runtime, &persist);
        assert!(moved.is_empty(), "nothing to adopt: {moved:?}");
        let second = SessionFiles::load(&persist, PSK).unwrap();
        assert_eq!(second.key.public, public);
        assert_eq!(second.pairing.psk, PSK);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_files_left_in_a_tmpfs_state_dir_are_adopted_once() {
        let root = std::env::temp_dir().join(format!("ldmux-adopt2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let persist = root.join("persist");
        let legacy = root.join("run");
        std::fs::create_dir_all(&persist).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();

        // A daemon from before the split: everything is in the runtime dir.
        let old = SessionFiles::load(&legacy, PSK).unwrap();
        let public = old.key.public;
        let mut pairing = old.current_pairing();
        pairing.pinned = Some([9; 32]);
        old.persister()(&pairing)
            .expect("the test's own directory accepts the pairing");

        let moved = adopt_legacy(&legacy, &persist);
        assert_eq!(moved.len(), 2, "{moved:?}");
        let adopted = SessionFiles::load(&persist, PSK).unwrap();
        assert_eq!(adopted.key.public, public, "the phone's pin still matches");
        assert_eq!(adopted.current_pairing().pinned, Some([9; 32]));

        // Idempotent: a second pass moves nothing.
        assert!(adopt_legacy(&legacy, &persist).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_files_are_0600_and_the_key_and_pin_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("ldmux-session-files-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let a = SessionFiles::load(&dir, PSK).unwrap();
        let mode = |n: &str| fs::metadata(dir.join(n)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("mesh-session.key"), 0o600);
        let mut persist = a.persister();
        persist(&PairingEntry {
            psk: PSK,
            pinned: Some([9; 32]),
            last_timestamp_ms: 77,
        })
        .unwrap();
        assert_eq!(mode("mesh-pairing.json"), 0o600);
        let b = SessionFiles::load(&dir, PSK).unwrap();
        assert_eq!(
            b.key.public, a.key.public,
            "the gateway key is stable across restarts"
        );
        assert_eq!(b.pairing.pinned, Some([9; 32]));
        assert_eq!(b.pairing.last_timestamp_ms, 77);
        b.clear_pin().unwrap();
        let c = SessionFiles::load(&dir, PSK).unwrap();
        assert_eq!(c.pairing.pinned, None);
        assert_eq!(c.pairing.last_timestamp_ms, 77, "the replay bar is kept");
        fs::write(dir.join("mesh-session.key"), b"short").unwrap();
        assert!(
            SessionFiles::load(&dir, PSK).is_err(),
            "a corrupt key is an error, not a new key"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

}
