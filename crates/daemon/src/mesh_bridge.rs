//! The shell bridge: the mesh terminal profile (ADR-0025) between a live shell and the L3
//! session.
//!
//! It reads **the keeper's grid** (the `snapshot` RPC, never raw PTY bytes), sends the phone
//! plain-text rows and then only the rows that changed, and writes the phone's `Input` to the
//! shell. It holds no socket and no clock of its own: [`Link`] is the session (the runtime in
//! [`crate::mesh`]) and [`Host`] is the daemon, so every rule here is testable without either.
//!
//! Rules that matter:
//!
//! - **One update in flight.** A screen is read and diffed only when the session has nothing
//!   queued. On a link that carries a record a minute, that yields fewer, newer updates; it
//!   never builds a queue of stale ones, and a phone that was away converges by one diff.
//! - **A failed send resets the diff.** If any message of an update cannot be queued, the
//!   differ forgets what the phone holds, so the next update is a full screen. The phone is
//!   never left holding half of a change the host believes it delivered.
//! - **A new handshake drops every attach.** The phone must `Attach` again; nothing carries over
//!   a session that ended.
//! - **Nothing here knows a radio.** The section 13 grep over the daemon crates stays at zero hits.

use std::collections::HashMap;
use std::sync::Arc;

use ldmux_protocol::keeper::SnapshotResult;
use ldmux_protocol::mesh_term::{
    DecodeError, Decoder, Differ, Message, Screen, VERSION, closed, session_pages,
};

/// The session as the bridge sees it.
pub trait Link {
    /// An L3 session is established.
    fn established(&self) -> bool;
    /// Changes whenever a new handshake is accepted.
    fn generation(&self) -> u32;
    /// Records still queued toward the phone.
    fn pending(&self) -> usize;
    fn send(&self, shell: u8, bytes: &[u8]) -> Result<(), String>;
    fn take_inbound(&self) -> Vec<(u8, Vec<u8>)>;
}

/// The daemon as the bridge sees it.
#[allow(async_fn_in_trait)]
pub trait Host {
    /// Ids of the running shells.
    async fn running(&mut self) -> Vec<String>;
    /// `(id, label)` of the running shells, for the phone's picker. The default has no labels.
    async fn listing(&mut self) -> Vec<(String, String)> {
        self.running()
            .await
            .into_iter()
            .map(|id| (id, String::new()))
            .collect()
    }
    async fn screen(&mut self, id: &str) -> Result<Screen, String>;
    async fn input(&mut self, id: &str, bytes: &[u8]) -> Result<(), String>;
}

impl Link for crate::mesh::MeshRuntime {
    fn established(&self) -> bool {
        self.snapshot().session.state == "established"
    }
    fn generation(&self) -> u32 {
        self.snapshot().session.handshakes
    }
    fn pending(&self) -> usize {
        self.snapshot().session.pending
    }
    fn send(&self, shell: u8, bytes: &[u8]) -> Result<(), String> {
        Self::send(self, shell, bytes)
    }
    fn take_inbound(&self) -> Vec<(u8, Vec<u8>)> {
        Self::take_inbound(self)
    }
}

/// What the bridge has done, for tests and for a future status field.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BridgeStats {
    pub updates: u32,
    pub messages_sent: u32,
    pub inputs: u32,
    pub malformed: u32,
    pub refused_sends: u32,
}

struct Attached {
    session: String,
    differ: Differ,
}

pub struct Bridge<H, L> {
    host: H,
    link: L,
    shells: HashMap<u8, Attached>,
    decoders: HashMap<u8, Decoder>,
    generation: u32,
    pub stats: BridgeStats,
}

impl<H: Host, L: Link> Bridge<H, L> {
    pub fn new(host: H, link: L) -> Self {
        Self {
            host,
            link,
            shells: HashMap::new(),
            decoders: HashMap::new(),
            generation: 0,
            stats: BridgeStats::default(),
        }
    }

    /// Whether `shell` is currently attached.
    #[must_use]
    pub fn attached(&self, shell: u8) -> bool {
        self.shells.contains_key(&shell)
    }

    fn reply(&mut self, shell: u8, messages: &[Message]) -> bool {
        let mut ok = true;
        for m in messages {
            let Ok(bytes) = m.encode() else {
                ok = false;
                continue;
            };
            if self.link.send(shell, &bytes).is_ok() {
                self.stats.messages_sent += 1;
            } else {
                self.stats.refused_sends += 1;
                ok = false;
                break;
            }
        }
        ok
    }

    /// One pass: take what the phone sent, then push what changed on screen.
    pub async fn tick(&mut self) {
        if !self.link.established() {
            self.shells.clear();
            self.decoders.clear();
            return;
        }
        let generation = self.link.generation();
        if generation != self.generation {
            self.generation = generation;
            self.shells.clear();
            self.decoders.clear();
        }

        for (shell, bytes) in self.link.take_inbound() {
            let decoded = self.decoders.entry(shell).or_default().push(&bytes);
            let items = match decoded {
                Ok(items) => items,
                Err(
                    DecodeError::BodyTooLong | DecodeError::Overflow | DecodeError::Malformed(_),
                ) => {
                    // The stream is unusable: drop the attach, the phone starts again.
                    self.stats.malformed += 1;
                    self.shells.remove(&shell);
                    self.decoders.remove(&shell);
                    continue;
                }
            };
            for item in items {
                match item {
                    Ok(m) => self.handle(shell, m).await,
                    Err(_) => self.stats.malformed += 1,
                }
            }
        }

        let ids: Vec<u8> = self.shells.keys().copied().collect();
        for shell in ids {
            if self.link.pending() > 0 {
                break;
            }
            let Some(session) = self.shells.get(&shell).map(|a| a.session.clone()) else {
                continue;
            };
            let screen = match self.host.screen(&session).await {
                Ok(s) => s,
                Err(_) => {
                    self.reply(
                        shell,
                        &[Message::Closed {
                            reason: closed::UNAVAILABLE,
                        }],
                    );
                    self.shells.remove(&shell);
                    continue;
                }
            };
            let messages = match self.shells.get_mut(&shell) {
                Some(a) => a.differ.update(&screen),
                None => continue,
            };
            if messages.is_empty() {
                continue;
            }
            self.stats.updates += 1;
            if !self.reply(shell, &messages) {
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.differ.reset();
                }
                continue;
            }
            if screen.dead {
                self.reply(
                    shell,
                    &[Message::Closed {
                        reason: closed::EXITED,
                    }],
                );
                self.shells.remove(&shell);
            }
        }
    }

    async fn handle(&mut self, shell: u8, m: Message) {
        match m {
            Message::Attach { version, selector } => {
                if version != VERSION {
                    self.reply(
                        shell,
                        &[Message::Closed {
                            reason: closed::VERSION,
                        }],
                    );
                    return;
                }
                let running = self.host.running().await;
                let matches: Vec<&String> = running
                    .iter()
                    .filter(|id| selector.is_empty() || id.starts_with(&selector))
                    .collect();
                match matches.as_slice() {
                    [] => {
                        self.reply(
                            shell,
                            &[Message::Closed {
                                reason: closed::NOT_FOUND,
                            }],
                        );
                    }
                    [one] => {
                        self.shells.insert(
                            shell,
                            Attached {
                                session: (*one).clone(),
                                differ: Differ::new(),
                            },
                        );
                    }
                    _ => {
                        self.reply(
                            shell,
                            &[Message::Closed {
                                reason: closed::AMBIGUOUS,
                            }],
                        );
                    }
                }
            }
            Message::List => {
                let listing = self.host.listing().await;
                self.reply(shell, &session_pages(&listing));
            }
            Message::Input(bytes) => {
                let Some(session) = self.shells.get(&shell).map(|a| a.session.clone()) else {
                    return;
                };
                self.stats.inputs += 1;
                let _ = self.host.input(&session, &bytes).await;
            }
            Message::Resync => {
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.differ.reset();
                }
            }
            Message::Detach => {
                self.shells.remove(&shell);
            }
            // Host-to-phone messages arriving from the phone are ignored.
            _ => {}
        }
    }
}

/// The daemon as a [`Host`]: every call is an RPC the control socket already serves.
pub struct DaemonHost(pub Arc<tokio::sync::Mutex<crate::daemon::LdmuxDaemon>>);

impl Host for DaemonHost {
    async fn running(&mut self) -> Vec<String> {
        let Ok(v) = self
            .0
            .lock()
            .await
            .handle("list", serde_json::json!({}))
            .await
        else {
            return Vec::new();
        };
        v.get("sessions")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter(|s| s.get("status").and_then(|x| x.as_str()) == Some("running"))
                    .filter_map(|s| s.get("id").and_then(|x| x.as_str()).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn listing(&mut self) -> Vec<(String, String)> {
        let Ok(v) = self
            .0
            .lock()
            .await
            .handle("list", serde_json::json!({}))
            .await
        else {
            return Vec::new();
        };
        v.get("sessions")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter(|s| s.get("status").and_then(|x| x.as_str()) == Some("running"))
                    .filter_map(|s| {
                        let id = s.get("id").and_then(|x| x.as_str())?;
                        let label = s.get("title").and_then(|x| x.as_str()).unwrap_or("");
                        Some((id.to_owned(), label.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn screen(&mut self, id: &str) -> Result<Screen, String> {
        let v = self
            .0
            .lock()
            .await
            .handle("snapshot", serde_json::json!({ "id": id }))
            .await?;
        let s: SnapshotResult = serde_json::from_value(v).map_err(|e| e.to_string())?;
        Ok(Screen::from_keeper(
            &s.text,
            s.cols,
            s.rows,
            (s.cursor_x, s.cursor_y),
            s.alternate_on,
            s.dead,
        ))
    }

    async fn input(&mut self, id: &str, bytes: &[u8]) -> Result<(), String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "input is not UTF-8".to_owned())?;
        self.0
            .lock()
            .await
            .handle("send", serde_json::json!({ "id": id, "text": text }))
            .await
            .map(|_| ())
    }
}

/// How often the bridge looks at the screen. Interactive echo wants this small, the airtime
/// bound does not care: an update waits for an empty queue regardless.
pub const TICK_MS: u64 = 1000;

/// Run the bridge for the life of the process.
pub async fn run<H: Host, L: Link>(mut bridge: Bridge<H, L>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(TICK_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        bridge.tick().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_protocol::mesh_term::{MAX_MESSAGE, Mirror};
    use std::sync::Mutex;

    #[derive(Default)]
    struct LinkState {
        established: bool,
        generation: u32,
        pending: usize,
        sent: Vec<(u8, Vec<u8>)>,
        inbound: Vec<(u8, Vec<u8>)>,
        refuse: bool,
    }
    #[derive(Clone, Default)]
    struct FakeLink(Arc<Mutex<LinkState>>);
    impl Link for FakeLink {
        fn established(&self) -> bool {
            self.0.lock().unwrap().established
        }
        fn generation(&self) -> u32 {
            self.0.lock().unwrap().generation
        }
        fn pending(&self) -> usize {
            self.0.lock().unwrap().pending
        }
        fn send(&self, shell: u8, bytes: &[u8]) -> Result<(), String> {
            let mut s = self.0.lock().unwrap();
            if s.refuse {
                return Err("full".into());
            }
            s.sent.push((shell, bytes.to_vec()));
            Ok(())
        }
        fn take_inbound(&self) -> Vec<(u8, Vec<u8>)> {
            std::mem::take(&mut self.0.lock().unwrap().inbound)
        }
    }

    #[derive(Default)]
    struct HostState {
        running: Vec<String>,
        screens: HashMap<String, Screen>,
        typed: Vec<(String, Vec<u8>)>,
        fail_screen: bool,
    }
    #[derive(Clone, Default)]
    struct FakeHost(Arc<Mutex<HostState>>);
    impl Host for FakeHost {
        async fn running(&mut self) -> Vec<String> {
            self.0.lock().unwrap().running.clone()
        }
        async fn listing(&mut self) -> Vec<(String, String)> {
            let s = self.0.lock().unwrap();
            s.running
                .iter()
                .map(|id| (id.clone(), format!("label of {id}")))
                .collect()
        }
        async fn screen(&mut self, id: &str) -> Result<Screen, String> {
            let s = self.0.lock().unwrap();
            if s.fail_screen {
                return Err("keeper gone".into());
            }
            s.screens.get(id).cloned().ok_or_else(|| "none".into())
        }
        async fn input(&mut self, id: &str, bytes: &[u8]) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .typed
                .push((id.to_owned(), bytes.to_vec()));
            Ok(())
        }
    }

    fn screen(lines: &[&str]) -> Screen {
        Screen {
            cols: 80,
            rows: lines.len() as u8,
            cursor_x: 0,
            cursor_y: 0,
            alternate: false,
            dead: false,
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn setup() -> (Bridge<FakeHost, FakeLink>, FakeHost, FakeLink) {
        let host = FakeHost::default();
        host.0.lock().unwrap().running = vec!["abcd-1".into()];
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["$", ""]));
        let link = FakeLink::default();
        {
            let mut l = link.0.lock().unwrap();
            l.established = true;
            l.generation = 1;
        }
        (Bridge::new(host.clone(), link.clone()), host, link)
    }

    fn phone_says(link: &FakeLink, shell: u8, m: &Message) {
        link.0
            .lock()
            .unwrap()
            .inbound
            .push((shell, m.encode().unwrap()));
    }

    /// What the phone would hold after everything the host sent.
    fn phone_view(link: &FakeLink, shell: u8) -> Mirror {
        let mut d = Decoder::new();
        let mut m = Mirror::new();
        for (s, bytes) in link.0.lock().unwrap().sent.iter() {
            if *s == shell {
                for item in d.push(bytes).unwrap() {
                    m.apply(&item.unwrap());
                }
            }
        }
        m
    }

    #[tokio::test]
    async fn attach_then_snapshot_then_keystroke_then_changed_rows() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        assert!(b.attached(3));
        assert_eq!(
            phone_view(&link, 3).lines,
            ["$", ""],
            "the whole screen, as plain rows"
        );
        assert!(
            link.0
                .lock()
                .unwrap()
                .sent
                .iter()
                .all(|(_, m)| m.len() <= MAX_MESSAGE)
        );

        // A keystroke goes to the shell as typed.
        phone_says(&link, 3, &Message::Input(b"ls\r".to_vec()));
        b.tick().await;
        assert_eq!(
            host.0.lock().unwrap().typed,
            vec![("abcd-1".to_owned(), b"ls\r".to_vec())]
        );

        // The shell answers; only the changed row crosses.
        let sent_before = link.0.lock().unwrap().sent.len();
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["$ ls", "file.txt"]));
        b.tick().await;
        assert_eq!(phone_view(&link, 3).lines, ["$ ls", "file.txt"]);
        let new = link.0.lock().unwrap().sent[sent_before..].to_vec();
        assert_eq!(
            new.len(),
            2,
            "one Rows and one Commit: no Profile, no full screen"
        );
    }

    #[tokio::test]
    async fn nothing_is_sent_when_nothing_changed_or_the_queue_is_busy() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            0,
            &Message::Attach {
                version: VERSION,
                selector: "abcd".into(),
            },
        );
        b.tick().await;
        let n = link.0.lock().unwrap().sent.len();
        b.tick().await;
        assert_eq!(link.0.lock().unwrap().sent.len(), n, "unchanged");

        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["changed", ""]));
        link.0.lock().unwrap().pending = 2;
        b.tick().await;
        assert_eq!(
            link.0.lock().unwrap().sent.len(),
            n,
            "a busy session gets no new update"
        );
        link.0.lock().unwrap().pending = 0;
        b.tick().await;
        assert!(link.0.lock().unwrap().sent.len() > n);
    }

    #[tokio::test]
    async fn a_phone_that_was_away_gets_one_diff_of_the_present() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        link.0.lock().unwrap().pending = 1; // unreachable for a while
        for i in 0..30 {
            host.0
                .lock()
                .unwrap()
                .screens
                .insert("abcd-1".into(), screen(&[&format!("tick {i}"), "x"]));
            b.tick().await;
        }
        let n = link.0.lock().unwrap().sent.len();
        link.0.lock().unwrap().pending = 0;
        b.tick().await;
        assert_eq!(
            link.0.lock().unwrap().sent.len() - n,
            2,
            "one Rows, one Commit"
        );
        assert_eq!(phone_view(&link, 1).lines, ["tick 29", "x"]);
    }

    #[tokio::test]
    async fn a_refused_send_resets_so_the_phone_is_never_left_half_updated() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["A", "B"]));
        link.0.lock().unwrap().refuse = true;
        b.tick().await;
        assert!(b.stats.refused_sends >= 1);
        link.0.lock().unwrap().refuse = false;
        b.tick().await;
        assert_eq!(
            phone_view(&link, 1).lines,
            ["A", "B"],
            "converged after the reset"
        );
    }

    #[tokio::test]
    async fn selectors_are_exact_about_not_found_and_ambiguous() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().running = vec!["abcd-1".into(), "abcd-2".into()];
        phone_says(
            &link,
            0,
            &Message::Attach {
                version: VERSION,
                selector: "zzzz".into(),
            },
        );
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd".into(),
            },
        );
        phone_says(
            &link,
            2,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: 9,
                selector: String::new(),
            },
        );
        b.tick().await;
        let sent = link.0.lock().unwrap().sent.clone();
        let reason = |shell: u8| {
            let mut d = Decoder::new();
            let (_, bytes) = sent.iter().find(|(s, _)| *s == shell).unwrap();
            match d.push(bytes).unwrap().remove(0).unwrap() {
                Message::Closed { reason } => reason,
                m => panic!("{m:?}"),
            }
        };
        assert_eq!(reason(0), closed::NOT_FOUND);
        assert_eq!(reason(1), closed::AMBIGUOUS);
        assert_eq!(reason(2), closed::AMBIGUOUS);
        assert_eq!(reason(3), closed::VERSION);
        assert!(!b.attached(0) && !b.attached(1) && !b.attached(2) && !b.attached(3));
    }

    /// Every `Sessions` page the host sent on `shell`.
    fn sessions_seen(link: &FakeLink, shell: u8) -> Vec<Message> {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for (s, bytes) in link.0.lock().unwrap().sent.iter() {
            if *s == shell {
                for item in d.push(bytes).unwrap() {
                    if let m @ Message::Sessions { .. } = item.unwrap() {
                        out.push(m);
                    }
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn list_answers_with_every_running_shell_and_never_guesses() {
        for (n, expect_pages_min) in [(0usize, 1usize), (1, 1), (6, 2)] {
            let (mut b, host, link) = setup();
            host.0.lock().unwrap().running = (0..n)
                .map(|i| format!("{i:02}{}", "e".repeat(30)))
                .collect();
            phone_says(&link, 4, &Message::List);
            b.tick().await;
            let pages = sessions_seen(&link, 4);
            assert!(
                pages.len() >= expect_pages_min,
                "n={n}: {} pages",
                pages.len()
            );
            let mut ids = Vec::new();
            for (i, m) in pages.iter().enumerate() {
                let Message::Sessions { last, entries } = m else {
                    unreachable!()
                };
                assert_eq!(*last, i + 1 == pages.len());
                ids.extend(entries.iter().map(|e| e.id.clone()));
            }
            assert_eq!(ids.len(), n, "n={n}");
            // Listing attaches nothing: the phone must choose.
            assert!(!b.attached(4), "n={n}: a list is not an attach");
        }
    }

    #[tokio::test]
    async fn the_chosen_id_attaches_what_was_listed() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().running = vec!["abcd-1".into(), "abcd-2".into()];
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-2".into(), screen(&["two"]));
        phone_says(&link, 4, &Message::List);
        b.tick().await;
        let Message::Sessions { entries, .. } = &sessions_seen(&link, 4)[0] else {
            panic!()
        };
        assert_eq!(entries[1].id, "abcd-2");
        phone_says(
            &link,
            4,
            &Message::Attach {
                version: 1,
                selector: entries[1].id.clone(),
            },
        );
        b.tick().await;
        assert!(b.attached(4));
    }

    #[tokio::test]
    async fn input_before_attach_is_ignored_and_never_reaches_a_shell() {
        let (mut b, host, link) = setup();
        phone_says(&link, 5, &Message::Input(b"rm -rf /\r".to_vec()));
        b.tick().await;
        assert!(host.0.lock().unwrap().typed.is_empty());
    }

    #[tokio::test]
    async fn a_new_handshake_drops_every_attach() {
        let (mut b, _host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        assert!(b.attached(1));
        link.0.lock().unwrap().generation = 2;
        b.tick().await;
        assert!(!b.attached(1), "the phone must attach again");
    }

    #[tokio::test]
    async fn a_corrupt_stream_drops_the_attach_and_a_bad_message_does_not() {
        let (mut b, _host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        // One malformed body (Closed with 2 bytes) costs one message only.
        link.0
            .lock()
            .unwrap()
            .inbound
            .push((1, vec![0x84, 0x02, 0, 0]));
        b.tick().await;
        assert!(b.attached(1));
        assert_eq!(b.stats.malformed, 1);
        // A length over the cap ends the attach.
        link.0
            .lock()
            .unwrap()
            .inbound
            .push((1, vec![0x02, 0xAD, 0x02]));
        b.tick().await;
        assert!(!b.attached(1));
    }

    #[tokio::test]
    async fn resync_resends_the_whole_screen() {
        let (mut b, _host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        let n = link.0.lock().unwrap().sent.len();
        phone_says(&link, 1, &Message::Resync);
        b.tick().await;
        let after = link.0.lock().unwrap().sent[n..].to_vec();
        let mut d = Decoder::new();
        let first = d.push(&after[0].1).unwrap().remove(0).unwrap();
        assert!(
            matches!(first, Message::Profile { .. }),
            "a full screen starts with Profile"
        );
    }

    #[tokio::test]
    async fn a_dead_shell_is_reported_and_detached_and_an_unreadable_one_too() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        let mut s = screen(&["bye", ""]);
        s.dead = true;
        host.0.lock().unwrap().screens.insert("abcd-1".into(), s);
        b.tick().await;
        assert!(!b.attached(1));
        assert_eq!(phone_view(&link, 1).closed, Some(closed::EXITED));

        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        host.0.lock().unwrap().fail_screen = true;
        b.tick().await;
        assert_eq!(phone_view(&link, 1).closed, Some(closed::UNAVAILABLE));
    }

    #[tokio::test]
    async fn no_session_means_no_attaches() {
        let (mut b, _host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        link.0.lock().unwrap().established = false;
        b.tick().await;
        assert!(!b.attached(1));
    }
}
