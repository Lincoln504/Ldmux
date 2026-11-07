//! Naming a shell after the conversation in it.
//!
//! **A row that says the folder says nothing.** The app's list and `ldmux list` name a shell by
//! its kind, else its directory's basename, so four agents in four directories read as four
//! folders and the operator has to remember which one is the release audit and which one is the
//! radio bug. The conversation knows its own subject; this is how it says so: `ldmux title "Radio
//! paths"`, one to three words, and the row says that instead.
//!
//! **Two names, one word set.** The shell's title is the daemon's (it is what every client draws),
//! and the agent's own session name is a fact of the agent's process, on disk. A `ldmux title`
//! therefore sets the daemon's title and *best effort* renames the agent too, because a phone row
//! and a `pi --resume` picker disagreeing about the same conversation is one name too many. The
//! agent side never fails the command: the operator's row is the product, the agent's picker is a
//! courtesy.
//!
//! **The record shapes are the agents' own, read from their code, not invented here:**
//!
//! - pi, `dist/core/session-manager.js` (`appendSessionInfo`): one appended line
//!   `{"type":"session_info","id":…,"parentId":…,"timestamp":…,"name":"…"}`, and the *last*
//!   `session_info` in the file wins on load. The file is named by `$PI_SESSION_FILE`.
//! - Claude Code, `claude` 2.1.317: `/rename` appends
//!   `{"type":"custom-title","customTitle":"…","sessionId":"…"}` to
//!   `~/.claude/projects/<cwd-with-dashes>/<sessionId>.jsonl`, and `custom-title` is last-wins;
//!   the newest session for a directory is `projects.<cwd>.lastSessionId` in `~/.claude.json`.
//!
//! Both files are append-only, and both are appended to with a single write on a file opened
//! `O_APPEND`, so a rename lands as one whole line between the agent's own lines. A file that does
//! not end in a newline is not touched: the last line is the agent's, half-written, and appending
//! after it would join the two.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The most words a title may have. Three is a subject; four is a sentence.
pub const WORDS_MAX: usize = 3;

/// The longest title kept, in bytes. It is drawn in a launcher row on a phone and sent in a session
/// entry on the radio, so it is a label, not a description.
pub const TITLE_MAX: usize = 24;

/// Why a title was refused, in the sentence the CLI prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    Empty,
    TooManyWords { words: usize },
    TooLong { bytes: usize },
    Control,
}

impl Refused {
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::Empty => "a title needs at least one word".to_string(),
            Self::TooManyWords { words } => format!(
                "that is {words} words; a title is {WORDS_MAX} at most"
            ),
            Self::TooLong { bytes } => format!(
                "that is {bytes} bytes; a title is {TITLE_MAX} at most"
            ),
            Self::Control => "a title is one line of printable text".to_string(),
        }
    }
}

/// The title as it will be stored, or why it will not be.
///
/// Whitespace is collapsed first, because a title typed with a trailing space or a tab is the same
/// title. Nothing else is rewritten: an operator who writes `MeshCore` gets `MeshCore`.
pub fn sanitize(words: &[&str]) -> Result<String, Refused> {
    let joined = words
        .iter()
        .flat_map(|w| w.split_whitespace())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>();
    if joined.is_empty() {
        return Err(Refused::Empty);
    }
    let title = joined.join(" ");
    if title.chars().any(|c| c.is_control()) {
        return Err(Refused::Control);
    }
    if joined.len() > WORDS_MAX {
        return Err(Refused::TooManyWords {
            words: joined.len(),
        });
    }
    if title.len() > TITLE_MAX {
        return Err(Refused::TooLong {
            bytes: title.len(),
        });
    }
    Ok(title)
}

/// What an agent-side rename did, for the CLI's one line of feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentName {
    /// No agent session was found for this shell: nothing to rename.
    None,
    /// The agent's own session now carries the title.
    Named,
    /// An agent session was found but the write did not happen (see the log line the caller writes).
    Failed,
}

/// Rename whichever agent session this shell is running, if one can be identified.
///
/// `cwd` is the shell's own directory, used for Claude Code's project folder; `env` is read for
/// pi's session file. Nothing here is authoritative: a shell with no agent, an agent whose format
/// changed, and a read-only home directory all end in [`AgentName::None`] or
/// [`AgentName::Failed`] and the shell's title still changed.
pub fn name_agent_session(title: &str, cwd: &str, env: &dyn Fn(&str) -> Option<String>) -> AgentName {
    if let Some(file) = env("PI_SESSION_FILE") {
        let path = PathBuf::from(file);
        return if path.is_file() {
            match append_pi_name(&path, title) {
                Ok(()) => AgentName::Named,
                Err(_) => AgentName::Failed,
            }
        } else {
            AgentName::None
        };
    }
    match claude_session_file(cwd, env) {
        Some(path) => match append_claude_title(&path, title) {
            Ok(()) => AgentName::Named,
            Err(_) => AgentName::Failed,
        },
        None => AgentName::None,
    }
}

/// Append pi's `session_info` record: the shape `appendSessionInfo` writes.
fn append_pi_name(path: &Path, title: &str) -> std::io::Result<()> {
    let last = last_line(path)?;
    // The parent is the file's current leaf, so the tree stays connected; a file whose last line
    // is not JSON gets no parent rather than a made-up one.
    let parent: Option<String> = serde_json::from_str::<serde_json::Value>(&last)
        .ok()
        .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_owned));
    let record = serde_json::json!({
        "type": "session_info",
        "id": record_id(),
        "parentId": parent,
        "timestamp": iso_now(),
        "name": title,
    });
    append_line(path, &record.to_string())
}

/// Append Claude Code's `custom-title` record, with the session id the transcript is named by.
fn append_claude_title(path: &Path, title: &str) -> std::io::Result<()> {
    let Some(session) = path.file_stem().and_then(|s| s.to_str()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "transcript has no session id",
        ));
    };
    let record = serde_json::json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": session,
    });
    append_line(path, &record.to_string())
}

/// Claude Code's transcript for `cwd`, when one is fresh enough to be the live session.
///
/// `~/.claude.json` records `projects.<cwd>.lastSessionId`, and the transcript is
/// `~/.claude/projects/<cwd with / as ->/<sessionId>.jsonl`. The freshness test is what keeps this
/// from naming an old conversation in the same folder: a title written to a transcript nobody is
/// working in is a rename the operator did not ask for.
fn claude_session_file(cwd: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let home = env("HOME")?;
    let root = PathBuf::from(home).join(".claude");
    let index: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join(".claude.json")).ok()?).ok()?;
    let id = index
        .get("projects")?
        .get(cwd)?
        .get("lastSessionId")?
        .as_str()?
        .to_owned();
    let slug: String = cwd
        .chars()
        .map(|c| if c == '/' { '-' } else { c })
        .collect();
    let file = root.join("projects").join(slug).join(format!("{id}.jsonl"));
    let fresh = file.metadata().ok()?.modified().ok()?;
    let age = std::time::SystemTime::now()
        .duration_since(fresh)
        .ok()?
        .as_secs();
    (age <= FRESH_SECS && file.is_file()).then_some(file)
}

/// How long a transcript may be untouched and still be "the conversation in this shell".
///
/// An agent that is thinking for eleven minutes is still the conversation; an agent that stopped
/// last week is not. The window is deliberately generous, because the cost of being wrong is only
/// that a stale conversation's picker shows a newer name.
const FRESH_SECS: u64 = 30 * 60;

/// The last line of `path`, without its newline. Empty when the file is empty.
fn last_line(path: &Path) -> std::io::Result<String> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .rsplit('\n')
        .nth(1.min(text.matches('\n').count()))
        .unwrap_or("")
        .trim_end()
        .to_string())
}

/// Append one JSON line, atomically with respect to the agent's own appends.
///
/// `O_APPEND` is what makes this safe: the kernel puts this write at the end of the file whatever
/// the agent is doing, and a single `write` of one short line does not interleave with another.
/// The newline check is the other half: a file whose last byte is not `\n` has a half-written line,
/// and a second line started there would corrupt both.
fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let bytes = std::fs::read(path)?;
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file does not end at a line boundary",
        ));
    }
    let mut f = OpenOptions::new().append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_data()
}

/// A short record id, in the shape the agents' own ids take (eight hex characters).
fn record_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:08x}", nanos ^ std::process::id())
}

/// An ISO-8601 timestamp, UTC, to the second: what both agents write.
fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days from the civil epoch, by the usual algorithm; no chrono in this workspace.
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// `(year, month, day)` for a count of days since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_one_to_three_words() {
        assert_eq!(sanitize(&["Radio", "paths"]).unwrap(), "Radio paths");
        assert_eq!(sanitize(&["  audio   drift "]).unwrap(), "audio drift");
        assert_eq!(sanitize(&["a", "b", "c"]).unwrap(), "a b c");
        assert_eq!(
            sanitize(&["a", "b", "c", "d"]),
            Err(Refused::TooManyWords { words: 4 })
        );
    }

    #[test]
    fn an_empty_title_is_refused_by_name() {
        assert_eq!(sanitize(&[]), Err(Refused::Empty));
        assert_eq!(sanitize(&["", "  "]), Err(Refused::Empty));
    }

    #[test]
    fn a_title_is_a_label_not_a_sentence() {
        let long = "x".repeat(TITLE_MAX + 1);
        assert_eq!(
            sanitize(&[&long]),
            Err(Refused::TooLong {
                bytes: TITLE_MAX + 1
            })
        );
        assert!(sanitize(&[&"x".repeat(TITLE_MAX)]).is_ok());
    }

    #[test]
    fn a_control_character_is_refused() {
        assert_eq!(sanitize(&["two\nlines"]), Ok("two lines".to_string()));
        assert_eq!(sanitize(&["bell\u{7}"]), Err(Refused::Control));
    }

    #[test]
    fn the_timestamp_is_iso_and_utc() {
        // 2026-11-07T00:00:00Z is 1794009600: the format is what matters here, not the clock.
        assert_eq!(iso_now().len(), 20);
        assert!(iso_now().ends_with('Z'));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
    }

    #[test]
    fn pi_gets_a_session_info_record_with_the_files_own_leaf_as_parent() {
        let dir = std::env::temp_dir().join(format!("ldmux-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("s.jsonl");
        std::fs::write(
            &file,
            "{\"type\":\"session\",\"id\":\"abc\"}\n{\"type\":\"message\",\"id\":\"deadbeef\"}\n",
        )
        .unwrap();
        append_pi_name(&file, "Radio paths").unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last["type"], "session_info");
        assert_eq!(last["name"], "Radio paths");
        assert_eq!(last["parentId"], "deadbeef", "the tree stays connected");
        assert!(last["timestamp"].as_str().unwrap().ends_with('Z'));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_half_written_last_line_is_not_appended_to() {
        let dir = std::env::temp_dir().join(format!("ldmux-title-half-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("s.jsonl");
        std::fs::write(&file, "{\"type\":\"session\"}\n{\"type\":\"mes").unwrap();
        assert!(append_pi_name(&file, "Radio paths").is_err());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\"type\":\"session\"}\n{\"type\":\"mes",
            "the agent's half-written line is left exactly as it was"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn claude_gets_a_custom_title_record_with_its_own_session_id() {
        let dir = std::env::temp_dir().join(format!("ldmux-title-claude-{}", std::process::id()));
        let projects = dir.join(".claude").join("projects").join("-work-repo");
        std::fs::create_dir_all(&projects).unwrap();
        let file = projects.join("11111111-2222-3333-4444-555555555555.jsonl");
        std::fs::write(&file, "{\"type\":\"mode\"}\n").unwrap();
        std::fs::write(
            dir.join(".claude").join(".claude.json"),
            format!(
                "{{\"projects\":{{\"/work/repo\":{{\"lastSessionId\":\"{}\"}}}}}}",
                file.file_stem().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        let home = dir.to_string_lossy().into_owned();
        let env = move |k: &str| (k == "HOME").then(|| home.clone());
        let found = claude_session_file("/work/repo", &env).expect("the transcript is found");
        assert_eq!(found, file);
        append_claude_title(&found, "Radio paths").unwrap();
        let text = std::fs::read_to_string(&found).unwrap();
        let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last["type"], "custom-title");
        assert_eq!(last["customTitle"], "Radio paths");
        assert_eq!(last["sessionId"], "11111111-2222-3333-4444-555555555555");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_old_transcript_is_not_renamed() {
        let dir = std::env::temp_dir().join(format!("ldmux-title-old-{}", std::process::id()));
        let projects = dir.join(".claude").join("projects").join("-work-old");
        std::fs::create_dir_all(&projects).unwrap();
        let file = projects.join("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
        std::fs::write(&file, "{\"type\":\"mode\"}\n").unwrap();
        std::fs::write(
            dir.join(".claude").join(".claude.json"),
            format!(
                "{{\"projects\":{{\"/work/old\":{{\"lastSessionId\":\"{}\"}}}}}}",
                file.file_stem().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        // Backdate the transcript past the freshness window.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(FRESH_SECS + 60);
        let handle = std::fs::File::options().write(true).open(&file).unwrap();
        handle.set_modified(old).unwrap();
        drop(handle);
        let home = dir.to_string_lossy().into_owned();
        let env = move |k: &str| (k == "HOME").then(|| home.clone());
        assert_eq!(claude_session_file("/work/old", &env), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_shell_with_no_agent_names_nothing() {
        let env = |_: &str| None;
        assert_eq!(
            name_agent_session("Radio paths", "/nowhere", &env),
            AgentName::None
        );
    }
}
