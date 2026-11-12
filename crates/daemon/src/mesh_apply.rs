//! What a `Send` is owed: every item of one command, in the order it was composed.
//!
//! A version-15 phone puts a whole draft in one message — typed text, a paste, and named keys —
//! because sending them as `Hold(false)`, then one `Input`, then one `Key` was three records and
//! three chances to lose a piece of one keystroke-and-enter, in any order
//! (`docs/RADIO-AUDIT-REVIEW.md` `RC-8`). One record on the air, however, is not one write on the
//! host: text is bytes for the pty, a paste is bytes for the pty encoded for the pane's own paste
//! mode, and a key is a name the **keeper** encodes against the pane's live cursor-key and keyboard
//! mode, which only the keeper can read.
//!
//! So a `Send` becomes an ordered plan of steps, and the caller runs them in order. The plan is
//! pure: it decides what each item means and where the boundaries are, and nothing about how a step
//! is performed.
//!
//! Two properties are the whole point, and both are pinned by tests:
//!
//!   * **Order is the operator's composition order.** An `Enter` that the operator put after the
//!     text stays after it, and a paste that sat in the middle of a draft stays in the middle.
//!   * **A newline never runs a command the operator did not confirm.** Every item goes through the
//!     same newline rule [`crate::mesh_bridge::paste_bytes`] applies to `Input`, so a draft carrying
//!     a newline is inserted as text rather than executed line by line.

use ldmux_protocol::mesh_term::SendItem;

use crate::mesh_bridge::paste_bytes;

/// Why a `Send` could not be turned into a plan.
///
/// Nothing is written when this is returned: a partially applied command is worse than a refused
/// one, because the operator cannot see what landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// An item with nothing in it. The wire refuses these too; a caller that built one by hand is
    /// told rather than spending a keeper call on nothing.
    Empty,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("an item of the command was empty"),
        }
    }
}

impl std::error::Error for ApplyError {}

/// What the pane says about itself, as far as a command's meaning depends on it.
///
/// A single field with a name, rather than a bare `bool` at the call site: the two facts a caller
/// holds at that point are whether the pane negotiated bracketed paste and which shell it is, and a
/// transposed `bool` would wrap a typed `Enter` in paste markers — the shell would insert a newline
/// instead of running the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneModes {
    /// The pane negotiated bracketed paste (`CSI ? 2004 h`), read from its last screen.
    pub bracketed_paste: bool,
}

/// One thing the host owes the pane, in the order the operator composed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStep {
    /// Bytes for the pty, already encoded for the pane's paste mode.
    Write(Vec<u8>),
    /// Key names for the keeper to encode against the pane's live mode.
    Keys(Vec<String>),
}

/// Turn one `Send` into the steps the host performs, in order.
///
/// Adjacent content items are joined **before** the paste rule runs, and then applied once, so a
/// draft of five runs costs one keeper call and one bracketed-paste wrapper rather than five.
///
/// That ordering is load bearing now that a long paste is several items: `MAX_SEND_ITEM` is 400
/// bytes, so a 2 kB paste arrives as five `Paste` items in a row, and a rule applied per item would
/// open and close the bracketed-paste region five times. The text is identical either way when the
/// pane negotiated bracketed paste, and when it did not the newline rule is applied to the whole run
/// exactly as it would have been to one item -- but one wrapper is what the operator's clipboard
/// actually means, and it keeps the bytes down.
///
/// Keys are never joined with content, and a key breaks the run on purpose: the text after it must
/// not be written before the key has been, or the order the operator composed is lost.
///
/// # Errors
///
/// [`ApplyError::Empty`] for an item that would say nothing: an empty text run, an empty paste, or
/// a key item with no names. Nothing partial is returned, so a caller that fails here writes
/// nothing at all.
pub fn apply_send(
    items: &[SendItem],
    pane: PaneModes,
) -> Result<Vec<ApplyStep>, ApplyError> {
    if items.is_empty() {
        return Err(ApplyError::Empty);
    }
    // Runs first, bytes second: adjacent content is concatenated as raw bytes, and only then does
    // the paste rule see one run. The alternative -- encode each item, then join -- puts a
    // bracketed-paste wrapper around every item, and a long paste is many items.
    enum Run<'a> {
        Content {
            /// Whether this run was pasted, which decides its newline rule.
            paste: bool,
            parts: Vec<&'a [u8]>,
        },
        Keys(Vec<&'a str>),
    }
    let mut runs: Vec<Run<'_>> = Vec::with_capacity(items.len());
    for item in items {
        match item {
            // **Text goes through the paste rule too.** Typed characters contain no newline -- a
            // newline in a draft is a key, and the phone sends it as one -- so the normal case is
            // unchanged, and the abnormal one is the case worth being careful about: a text run
            // that does carry a newline would otherwise run its first line as a command, which is
            // exactly the failure this whole message kind exists to remove.
            SendItem::Text(bytes) | SendItem::Paste(bytes) => {
                if bytes.is_empty() {
                    return Err(ApplyError::Empty);
                }
                let paste = matches!(item, SendItem::Paste(_));
                // **Same kind only.** A typed run and a paste are separate on purpose: typed text
                // is the operator's own keystrokes and a paste is a block they did not type, and
                // joining them would put the keystrokes inside the paste region. Two pastes in a
                // row are one paste, which is the case chunking creates.
                match runs.last_mut() {
                    Some(Run::Content { paste: was, parts }) if *was == paste => parts.push(bytes),
                    _ => runs.push(Run::Content {
                        paste,
                        parts: vec![bytes],
                    }),
                }
            }
            SendItem::Keys(names) => {
                if names.is_empty() {
                    return Err(ApplyError::Empty);
                }
                match runs.last_mut() {
                    Some(Run::Keys(prev)) => prev.extend(names.iter().map(String::as_str)),
                    _ => runs.push(Run::Keys(names.iter().map(String::as_str).collect())),
                }
            }
        }
    }

    let mut steps: Vec<ApplyStep> = Vec::with_capacity(runs.len());
    for run in runs {
        match run {
            Run::Content { parts, .. } => {
                let joined: Vec<u8> = parts.concat();
                // A paste is wrapped for the pane's bracketed-paste mode so its newlines never run;
                // typed text takes the same rule, because a text run carrying a newline would
                // otherwise run its first line as a command. `paste_bytes` is the one place that
                // decides, and it sees a whole run rather than an item: that is what gives a long
                // paste one wrapper instead of one per 400-byte item.
                let encoded = paste_bytes(&joined, pane.bracketed_paste);
                // Adjacent writes stay one step, so a draft of five runs is one keeper call.
                match steps.last_mut() {
                    Some(ApplyStep::Write(prev)) => prev.extend_from_slice(&encoded),
                    _ => steps.push(ApplyStep::Write(encoded)),
                }
            }
            Run::Keys(names) => {
                steps.push(ApplyStep::Keys(names.into_iter().map(str::to_owned).collect()));
            }
        }
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> PaneModes {
        PaneModes {
            bracketed_paste: false,
        }
    }

    fn bracketed() -> PaneModes {
        PaneModes {
            bracketed_paste: true,
        }
    }

    fn keys(names: &[&str]) -> SendItem {
        SendItem::Keys(names.iter().map(|n| (*n).to_owned()).collect())
    }

    /// **The command the operator typed is the bytes the shell used to receive.**
    ///
    /// `ls` plus `Enter` was `Input(b"ls\r")` on the version-14 wire; it must still reach the pty
    /// as exactly `ls` followed by a key call, so a shell cannot tell which protocol carried it.
    #[test]
    fn a_typed_command_is_the_same_bytes_as_the_old_input() {
        let steps = apply_send(
            &[SendItem::Text(b"ls".to_vec()), keys(&["Enter"])],
            plain(),
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![
                ApplyStep::Write(b"ls".to_vec()),
                ApplyStep::Keys(vec!["Enter".to_owned()]),
            ]
        );
    }

    /// Adjacent runs of the same kind cost one keeper call each, and the order inside them is the
    /// order they were composed in.
    #[test]
    fn adjacent_runs_merge_and_keep_their_order() {
        let steps = apply_send(
            &[
                SendItem::Text(b"cd ".to_vec()),
                SendItem::Text(b"/tmp".to_vec()),
                keys(&["Tab", "Enter"]),
            ],
            plain(),
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![
                ApplyStep::Write(b"cd /tmp".to_vec()),
                ApplyStep::Keys(vec!["Tab".to_owned(), "Enter".to_owned()]),
            ],
            "two text runs are one write, two keys are one key call"
        );
    }

    /// A key in the middle of a draft breaks the write in two: the text after it must not reach the
    /// pty before the key does.
    #[test]
    fn a_key_in_the_middle_splits_the_write() {
        let steps = apply_send(
            &[
                SendItem::Text(b"a".to_vec()),
                keys(&["Enter"]),
                SendItem::Text(b"b".to_vec()),
            ],
            plain(),
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![
                ApplyStep::Write(b"a".to_vec()),
                ApplyStep::Keys(vec!["Enter".to_owned()]),
                ApplyStep::Write(b"b".to_vec()),
            ]
        );
    }

    /// A multi-line paste reaches a pane that asked for the markers wrapped, so its lines are
    /// inserted at the prompt instead of run one after another — and one that did not ask gets
    /// carriage returns, because there is no other way to give it a line.
    #[test]
    fn a_paste_is_encoded_for_the_panes_own_mode() {
        let items = [SendItem::Paste(b"one\ntwo\n".to_vec())];
        assert_eq!(
            apply_send(&items, bracketed()).unwrap(),
            vec![ApplyStep::Write(b"\x1b[200~one\ntwo\n\x1b[201~".to_vec())]
        );
        assert_eq!(
            apply_send(&items, plain()).unwrap(),
            vec![ApplyStep::Write(b"one\rtwo\r".to_vec())]
        );
    }

    /// **A newline in a text run is not a command.** The phone sends an operator's `Enter` as a
    /// key, so a text run carrying a newline is a client that lost that distinction; inserting it
    /// is recoverable, running it is not.
    #[test]
    fn a_newline_in_a_text_run_is_inserted_rather_than_executed() {
        let steps = apply_send(
            &[SendItem::Text(b"echo one\necho two\n".to_vec())],
            bracketed(),
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![ApplyStep::Write(
                b"\x1b[200~echo one\necho two\n\x1b[201~".to_vec()
            )],
            "a text run with a newline is a paste, so neither line runs"
        );
    }

    /// A paste long enough to be several items is **one** bracketed-paste region.
    ///
    /// `MAX_SEND_ITEM` is 400 bytes, so a 2 kB paste arrives as five `Paste` items in a row
    /// (`docs/RADIO-PLAN.md` §12.2, AR-3). A rule applied per item would open and close the region
    /// five times: identical text when the pane negotiated bracketed paste, but five paste events
    /// where the operator's clipboard has one, and 60 bytes of markers that buy nothing.
    #[test]
    fn a_long_paste_is_one_bracketed_region() {
        // 400 bytes of two-byte lines: newline-bearing, because a paste that carries no newline is
        // written as it is and has nothing to wrap.
        let chunk = |n: u8| SendItem::Paste([n, b'\n'].repeat(200));
        let steps = apply_send(
            &[chunk(b'a'), chunk(b'b'), chunk(b'c'), keys(&["Enter"])],
            bracketed(),
        )
        .unwrap();
        let ApplyStep::Write(bytes) = &steps[0] else {
            panic!("the paste was not a write: {steps:?}");
        };
        let opens = bytes
            .windows(6)
            .filter(|w| *w == b"\x1b[200~")
            .count();
        let closes = bytes
            .windows(6)
            .filter(|w| *w == b"\x1b[201~")
            .count();
        assert_eq!((opens, closes), (1, 1), "a 1200-byte paste was wrapped {opens} times");
        assert_eq!(bytes.len(), 1200 + 12, "the paste lost or gained bytes: {}", bytes.len());
        assert_eq!(steps.len(), 2, "the paste and the key are two steps: {steps:?}");
    }

    /// A paste that sits in the middle of a draft keeps its position, marked and in order.
    #[test]
    fn a_paste_inside_a_draft_keeps_its_place() {
        let steps = apply_send(
            &[
                SendItem::Text(b"cat ".to_vec()),
                SendItem::Paste(b"x\ny\n".to_vec()),
                keys(&["Enter"]),
            ],
            bracketed(),
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![
                ApplyStep::Write(b"cat \x1b[200~x\ny\n\x1b[201~".to_vec()),
                ApplyStep::Keys(vec!["Enter".to_owned()]),
            ]
        );
    }

    /// A command with nothing in it is refused, and so is any single empty item: a refused command
    /// writes nothing, so the operator's screen stays true.
    #[test]
    fn an_empty_item_is_refused_and_nothing_is_planned() {
        assert_eq!(apply_send(&[], plain()), Err(ApplyError::Empty));
        assert_eq!(
            apply_send(&[SendItem::Text(Vec::new())], plain()),
            Err(ApplyError::Empty)
        );
        assert_eq!(
            apply_send(&[SendItem::Paste(Vec::new())], plain()),
            Err(ApplyError::Empty)
        );
        assert_eq!(
            apply_send(&[SendItem::Keys(Vec::new())], plain()),
            Err(ApplyError::Empty)
        );
        assert_eq!(
            apply_send(&[SendItem::Text(b"ok".to_vec()), SendItem::Keys(Vec::new())], plain()),
            Err(ApplyError::Empty),
            "an empty item anywhere refuses the whole command"
        );
    }
}
