# contrib

Optional pieces for the tools around ldmux. Nothing here is installed by `nix run .#install` and
nothing here is needed for ldmux itself to work.

## `skills/ldmux-shell`

An agent skill that names each shell after the conversation in it (`ldmux title "Radio paths"`) and
marks it when a turn ends (`ldmux buzz`), so the app's shell list reads as work in progress rather
than as a list of folders.

It is deliberately a **skill and not a hook**: what names and marks a shell is the verb, and inside a
shell both need no argument at all (`$LDMUX_SESSION_ID`). A harness that never runs the skill is
silent, and so is one with no hook; where a harness does expose a deterministic end-of-turn
lifecycle hook, calling the same verbs from it is the better integration and needs nothing from this
directory.

Install it for pi, or for any harness that reads the Agent Skills layout:

```sh
mkdir -p ~/.pi/agent/skills && cp -r contrib/skills/ldmux-shell ~/.pi/agent/skills/
```

Both verbs exit non-zero with one sentence when there is no daemon, no shell or an older host, and
the skill tells the agent to ignore that: a failed name or mark is not a failed turn.
