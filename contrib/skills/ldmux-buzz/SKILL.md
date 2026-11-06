---
name: ldmux-buzz
description: Tell the operator's phone when you finish a turn, from inside an ldmux shell. Use when the task is done and the operator may be away from the desk.
---

# Ring the phone at the end of a turn

`ldmux buzz` marks this shell in the ldmux app's shell list: a small square beside the row that says
the work finished (`done`) or that the shell is waiting on the operator (`attention`). It is not a
notification and it is not a write: it touches no claim, no pty and no output.

Run it **once**, at the end of a turn, and only when the operator is plausibly away from the desk:

```sh
ldmux buzz
```

The shell names itself: every child of an ldmux shell carries `LDMUX_SESSION_ID`, so no argument is
needed and the wrong shell cannot be marked by copying an id. From outside a shell, name one:

```sh
ldmux buzz --session <shell id> --kind attention --note "tests failed"
```

- `--kind done` (the default): an agent finished a turn.
- `--kind attention`: the shell wants a look — a question, an error, a prompt waiting.
- `--note "…"`: at most 80 bytes, one line, shown beside the row on a phone that asked for
  notifications. Optional, and the row mark appears either way.

**Never retry and never treat a failure as a failed turn.** No daemon, no shell, or a host older
than the marks feature all exit non-zero with one sentence, and the work is still done. A buzz inside
the same five seconds as the last one for this shell is refused on purpose: a mark is not a progress
bar, and a loop that buzzes per line is a phone that never stops ringing.

Do not buzz in the middle of work, per tool call, or for a turn the operator is watching.
