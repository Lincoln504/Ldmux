//! The host's answer contract for the companion command window.
//!
//! `ldmux_mesh::companion::CompanionFifo` enforces *one command in flight*. It cannot know which
//! answer belongs to which command, because the firmware echoes no sequence number, so the contract
//! is data and it lives here — beside the opcodes it names, in the crate that owns a radio.
//!
//! Every entry is read from the firmware's own dispatch (`handleCmdFrame`,
//! `examples/companion_radio/MyMesh.cpp:1055-2065`), and the line numbers below are the branch that
//! writes the answer. A command with a **typed** reply must be listed with it, or the reply arrives
//! as an unexpected frame and the command waits out its deadline; a command the firmware answers
//! with `OK` or an error needs no entry for the reply, because those codes are universal.
//!
//! Two things this table must not become:
//!
//! * **A list of commands the host does not send.** The phone sends `CMD_GET_CONTACT_BY_KEY`
//!   (`0x1E`), `CMD_GET_CHANNEL` (`0x1F`) and `CMD_GET_BATT_AND_STORAGE` (`0x14`), each of which can
//!   answer `RESP_CODE_ERR` with `ERR_CODE_NOT_FOUND`. Those belong to the phone's table when its
//!   engine moves into this tree (`docs/RADIO-PLAN.md` §1 `A2`); putting them here would be a claim
//!   about a client that is not this one.
//! * **A lookup keyed on one byte for two namespaces.** Command and answer codes are separate
//!   namespaces that happen to overlap: `CMD_SET_RADIO_TX_POWER` is `0x0C` and the phone's
//!   `RESP_CODE_BATT_AND_STORAGE` is also `0x0C`. [`HOST_COMMANDS`] is keyed by **command**; an
//!   answer is only ever compared against the `settles_on` sets, never looked up as a command.

use ldmux_mesh::companion::{CommandSpec, Entry, UNIVERSAL_ANSWERS};

use crate::codec::{codes, commands};

/// How long a command may wait for an answer: three figures, each with a reason rather than a
/// roundness.
///
/// - `PROMPT`: the branch writes its answer from the firmware's own loop with nothing to wait for.
/// - `FLASH`: the branch saves preferences to the store before answering, which is a flash write on
///   both boards and is the slowest thing this protocol does.
/// - `TRANSMIT`: the branch hands a packet to the radio and writes the answer immediately after; a
///   transmission deferred behind a busy channel is reported rather than waited for, so this is not
///   the airtime.
///
/// A command the table does not name falls back to `ldmux_mesh::companion::DEFAULT_TIMEOUT_MS`,
/// which is the same two thousand milliseconds — a command the firmware does not recognise, or one
/// a future host adds without a contract.
const PROMPT: u32 = 1_000;
const FLASH: u32 = 3_000;
const TRANSMIT: u32 = 2_000;

/// Every command the host sends, with the answers that complete it.
///
/// `SET_DEVICE_TIME`, `SET_RADIO_PARAMS`, `SET_RADIO_TX_POWER`, `SET_PATH_HASH_MODE`,
/// `SET_DEFAULT_FLOOD_SCOPE` and `SEND_SELF_ADVERT` have no typed reply: their branches end in
/// `writeOKFrame()` or `writeErrFrame(...)` (`MyMesh.cpp:1289-1300`, `:1426-1480`, `:1462-1500`,
/// `:1506-1518`, `:1991-2007`, `:1299-1312`), which the universal set already covers.
///
/// The six below do, and each one would otherwise expire:
///
/// * `DEVICE_QUERY` (`0x16`) → `RESP_CODE_DEVICE_INFO` (`MyMesh.cpp:1056-1075`).
/// * `APP_START` (`0x01`) → `RESP_CODE_SELF_INFO` (`:1076-1114`).
/// * `GET_ADVERT_PATH` (`0x2A`) → `RESP_CODE_ADVERT_PATH`, or `ERR_CODE_NOT_FOUND` when the cache has
///   no entry for that key, which is the ordinary answer for a peer never heard
///   (`:1895-1915`).
/// * `GET_STATS` (`0x38`) → `RESP_CODE_STATS` (`:1916-1990`).
/// * `GET_DEFAULT_FLOOD_SCOPE` (`0x40`) → `RESP_CODE_DEFAULT_FLOOD_SCOPE` (`:2008-2030`).
/// * `SEND_RAW_PACKET` (`0x41`) → `OK` when the packet parses, `ERR_CODE_ILLEGAL_ARG` when it does
///   not, `ERR_CODE_TABLE_FULL` when the radio's packet pool is empty, and `RESP_CODE_SENT` on
///   firmware that confirms sends (`:2047-2060`). All four are universal except `SENT`, which is the
///   one code that has to be named.
pub const HOST_COMMANDS: CommandSpec = CommandSpec {
    universal: UNIVERSAL_ANSWERS,
    entries: &[
        Entry::typed(commands::DEVICE_QUERY, &[codes::DEVICE_INFO], PROMPT),
        Entry::typed(commands::APP_START, &[codes::SELF_INFO], PROMPT),
        Entry::typed(
            commands::GET_ADVERT_PATH,
            &[codes::ADVERT_PATH],
            PROMPT,
        ),
        Entry::typed(commands::GET_STATS, &[codes::STATS], PROMPT),
        Entry::typed(
            commands::GET_DEFAULT_FLOOD_SCOPE,
            &[codes::DEFAULT_FLOOD_SCOPE],
            PROMPT,
        ),
        Entry::typed(commands::SEND_RAW_PACKET, &[codes::SENT], TRANSMIT),
        Entry::terminal(commands::SET_DEVICE_TIME, FLASH),
        Entry::terminal(commands::SEND_SELF_ADVERT, FLASH),
        Entry::terminal(commands::SET_RADIO_PARAMS, FLASH),
        Entry::terminal(commands::SET_RADIO_TX_POWER, FLASH),
        Entry::terminal(commands::SET_PATH_HASH_MODE, FLASH),
        Entry::terminal(commands::SET_DEFAULT_FLOOD_SCOPE, FLASH),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The table covers every command this crate and the daemon actually send, and nothing else.
    ///
    /// The list is written out rather than derived, because the point is to fail when a new command
    /// is added without an answer contract — a derived list would agree with itself.
    #[test]
    fn the_table_covers_exactly_the_commands_the_host_sends() {
        let sent = [
            commands::APP_START,
            commands::DEVICE_QUERY,
            commands::GET_ADVERT_PATH,
            commands::GET_DEFAULT_FLOOD_SCOPE,
            commands::GET_STATS,
            commands::SEND_RAW_PACKET,
            commands::SEND_SELF_ADVERT,
            commands::SET_DEVICE_TIME,
            commands::SET_PATH_HASH_MODE,
            commands::SET_RADIO_PARAMS,
            commands::SET_RADIO_TX_POWER,
            commands::SET_DEFAULT_FLOOD_SCOPE,
        ];
        for command in sent {
            assert!(
                HOST_COMMANDS.entry(command).is_some(),
                "the host sends {command:#04x} but the table has no answer contract for it"
            );
        }
        assert_eq!(HOST_COMMANDS.entries.len(), sent.len());
    }

    /// Each typed reply settles its own command and no other.
    #[test]
    fn a_typed_reply_settles_only_its_own_command() {
        let pairs = [
            (commands::DEVICE_QUERY, codes::DEVICE_INFO),
            (commands::APP_START, codes::SELF_INFO),
            (commands::GET_ADVERT_PATH, codes::ADVERT_PATH),
            (commands::GET_STATS, codes::STATS),
            (commands::GET_DEFAULT_FLOOD_SCOPE, codes::DEFAULT_FLOOD_SCOPE),
            (commands::SEND_RAW_PACKET, codes::SENT),
        ];
        for (command, answer) in pairs {
            assert!(
                HOST_COMMANDS.accepts(command, answer),
                "{command:#04x} is not settled by its own answer {answer:#04x}"
            );
            for (other, _) in pairs {
                if other != command {
                    assert!(
                        !HOST_COMMANDS.accepts(other, answer),
                        "{other:#04x} would be settled by {answer:#04x}, which is {command:#04x}'s"
                    );
                }
            }
        }
    }

    /// The universal codes settle everything, which is what makes a terminal command cheap to write
    /// and what makes one window enough.
    #[test]
    fn every_command_is_settled_by_a_terminal_code() {
        for entry in HOST_COMMANDS.entries {
            for answer in UNIVERSAL_ANSWERS {
                assert!(
                    HOST_COMMANDS.accepts(entry.command, *answer),
                    "{:#04x} does not accept the terminal code {answer:#04x}",
                    entry.command
                );
            }
        }
    }

    /// A command and an answer may share a byte value; the table keys on one and compares the other,
    /// so the overlap is harmless here and must stay harmless.
    ///
    /// `CMD_SET_RADIO_TX_POWER` is `0x0C`; the phone's `RESP_CODE_BATT_AND_STORAGE` is also `0x0C`.
    /// A frame with that byte does not settle the transmit-power command, because no answer in its
    /// set has that value.
    #[test]
    fn a_command_byte_that_is_also_an_answer_byte_settles_nothing() {
        assert_eq!(commands::SET_RADIO_TX_POWER, 0x0C);
        assert!(HOST_COMMANDS.entry(0x0C).is_some());
        assert!(!HOST_COMMANDS.accepts(commands::SET_RADIO_TX_POWER, 0x0C));
    }

    /// An unlisted command can still be settled, by a terminal code, and gets the default timeout —
    /// the firmware answers anything it does not recognise with `ERR_CODE_UNSUPPORTED_CMD`
    /// (`MyMesh.cpp:2061-2063`).
    #[test]
    fn an_unlisted_command_is_settled_by_a_terminal_code_and_gets_the_default_timeout() {
        const NOT_SENT: u8 = 0x09;
        assert!(HOST_COMMANDS.entry(NOT_SENT).is_none());
        assert!(HOST_COMMANDS.accepts(NOT_SENT, codes::ERROR));
        assert_eq!(
            HOST_COMMANDS.timeout_ms(NOT_SENT),
            ldmux_mesh::companion::DEFAULT_TIMEOUT_MS
        );
    }

    /// The timeouts are the ones the branches justify, so a change to one is visible.
    #[test]
    fn the_timeouts_match_the_branch_that_answers() {
        assert_eq!(HOST_COMMANDS.timeout_ms(commands::DEVICE_QUERY), PROMPT);
        assert_eq!(HOST_COMMANDS.timeout_ms(commands::APP_START), PROMPT);
        assert_eq!(HOST_COMMANDS.timeout_ms(commands::SEND_RAW_PACKET), TRANSMIT);
        assert_eq!(HOST_COMMANDS.timeout_ms(commands::SET_RADIO_PARAMS), FLASH);
    }
}
