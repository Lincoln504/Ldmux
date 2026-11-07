---
name: ldmux-shell
description: Say what this ldmux shell is working on, and tell the operator's phone when the turn ends. Use at the start of a task and again when it is finished.
---

# Name the shell, then mark it

A shell in the ldmux app is named after its folder until something names it better, so four agents
read as four paths and nobody can tell which one is the release audit. Two commands fix that, and
both are about the row on the operator's phone.

## Name it: once, when the subject is clear

```sh
ldmux title "Radio paths"
```

- **One to three words**, 24 bytes at most. A title is a label, not a description.
- The phone's row and `ldmux list` show it instead of the folder.
- Name the subject, not the tool and not the step: `Radio paths`, not `Bash`.
- When the shell is running a pi or Claude Code session, the same words also become that agent's
  own session name, so a `pi --resume` picker and the phone agree.
- **Rename when the subject changes.** A title that still says the old topic is worse than none.
- The shell names itself: every child of an ldmux shell carries `LDMUX_SESSION_ID`, so no argument
  is needed and the wrong shell cannot be named by copying an id. From outside a shell:
  `ldmux title --session <shell id> "Two words"`.

## Mark it: once, at the end of a turn

```sh
ldmux buzz
```

`ldmux buzz` marks this shell in the app's list: a small square beside the row saying the work
finished (`done`) or that the shell is waiting on the operator (`attention`). It is not a write: it
touches no claim, no pty and no output.

- `--kind done` (the default): an agent finished a turn.
- `--kind attention`: the shell wants a look — a question, an error, a prompt waiting.
- `--note "…"`: at most 80 bytes, one line, shown beside the row on a phone that asked for
  notifications. Optional, and the row mark appears either way.

Run it **once**, at the end of a turn, and only when the operator is plausibly away from the desk.
Do not buzz in the middle of work, per tool call, or for a turn the operator is watching.

**Never retry and never treat a failure as a failed turn.** No daemon, no shell, or a host older
than these features all exit non-zero with one sentence, and the work is still done. A buzz inside
the same five seconds as the last one for this shell is refused on purpose: a mark is not a progress
bar, and a loop that buzzes per line is a phone that never stops ringing.
