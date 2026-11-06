# contrib

Optional pieces for the tools around ldmux. Nothing here is installed by `nix run .#install` and
nothing here is needed for ldmux itself to work.

## `skills/ldmux-buzz`

An agent skill that tells a coding agent to run `ldmux buzz` once at the end of a turn, so the shell
shows up marked (`done`) in the app's shell list on the phone.

It is deliberately a **skill and not a hook**: what marks a shell is the verb, and inside a shell it
needs no argument at all (`$LDMUX_SESSION_ID`). A harness that never runs the skill is silent, and so
is one with no hook; where a harness does expose a deterministic end-of-turn lifecycle hook, calling
the same verb from it is the better integration and needs nothing from this directory.

Install it for pi, or for any harness that reads the Agent Skills layout:

```sh
mkdir -p ~/.pi/agent/skills && cp -r contrib/skills/ldmux-buzz ~/.pi/agent/skills/
```

`ldmux buzz` exits non-zero with one sentence when there is no daemon, no shell or an older host, and
the skill tells the agent to ignore that: a failed buzz is not a failed turn.
