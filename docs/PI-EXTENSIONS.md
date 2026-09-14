# pi extension development — internal reference

Distilled from a deep read of pi **0.85.1** docs, source, and every extension on
this machine (pi-research, pi-fork, pi-color, pi-blackhole). Kept here so ldmux's
extension (`packages/extension`) is written correctly. Source of truth is always
the installed package:

```
/home/ldeen/.config/nvm/versions/node/v25.8.2/lib/node_modules/@earendil-works/pi-coding-agent
  docs/extensions.md   docs/rpc.md   docs/sdk.md   docs/session-format.md
  dist/core/extensions/{loader,types,runner}.js   dist/core/agent-session.d.ts
  dist/modes/rpc/{rpc-mode,rpc-client,rpc-types,jsonl}.d.ts
```

## 1. Structure & loading

- **Entry point:** default export factory, sync or async: `export default function (pi: ExtensionAPI) {}` (async is awaited before `session_start`).
- **Location:** `~/.pi/agent/extensions/<name>/index.ts`, `~/.pi/agent/extensions/*.ts` (global); `.pi/extensions/...` (project-local, after trust); or `settings.json` `packages`/`extensions`; or `pi -e <path>`.
- A directory extension may declare its entry via `package.json`: `{ "pi": { "extensions": ["./src/index.ts"] } }`.
- Loaded with **jiti** — `.ts` runs with **no build step**. Relative `.ts` imports work; Node builtins (`node:net`, `node:child_process`) work.
- `/reload` hot-reloads auto-discovered extensions.
- **Two-phase registration:** API calls during the factory are *queued* and only committed after it returns (`loader.commit()`); on throw they are discarded. `sendMessage`/session mutation throw "not initialized" during the factory.

## 2. Registration API (`pi.*`)

| Call | Purpose |
|---|---|
| `pi.on(event, handler)` | Subscribe to lifecycle/agent/tool/input events |
| `pi.registerTool(def)` | Expose an LLM-callable tool |
| `pi.registerCommand(name, {description, handler, getArgumentCompletions?})` | Slash command (`/name`) |
| `pi.registerShortcut(key, {handler, description?})` | Keybinding |
| `pi.registerFlag(name, {type:"boolean"\|"string", default?})` / `pi.getFlag(name)` | CLI flags |
| `pi.registerProvider(name, config)` | Dynamic model provider |
| `pi.sendMessage(msg, opts)` / `pi.sendUserMessage(text, opts)` | Inject messages into the session |
| `pi.appendEntry(customType, data)` | Persist extension state in the session JSONL (not in LLM context) |
| `pi.setSessionName(name)` / `pi.getSessionName()` | Session display name |
| `pi.exec(cmd, args, {timeout?, signal?})` | Run a shell command |
| `pi.getActiveTools()` / `pi.getAllTools()` / `pi.setActiveTools(names)` | Tool registry |
| `pi.events.on(channel, h)` / `pi.events.emit(channel, data)` | Extension-to-extension event bus |

**Tool definition** (TypeBox schema; a plain JSON-Schema object also works):

```ts
pi.registerTool({
  name: "ldmux", label: "ldmux", description: "...",
  parameters: { type: "object", properties: { /* ... */ }, required: ["action"] },
  async execute(toolCallId, params, signal, onUpdate, ctx) {
    return { content: [{ type: "text", text: "..." }], details: {} };
  },
});
```

Tool names must be unique (first registration wins).

## 3. Events (high level)

Session: `session_start` `{reason: "startup"|"reload"|"new"|"resume"|"fork", previousSessionFile?}`;
`session_shutdown` `{reason: "quit"|"reload"|"new"|"resume"|"fork", targetSessionFile?}`;
`session_before_switch`, `session_before_fork`, `session_info_changed`,
`session_before_compact`, `session_compact`, `session_before_tree`, `session_tree`.

Agent/turn: `before_agent_start` (may rewrite `systemPrompt`), `agent_start`, `agent_end`, `agent_settled`, `turn_start`, `turn_end`, `message_start/update/end`.

Tools: `tool_call` (mutable `input`, can block), `tool_result` (can rewrite result), `tool_execution_start/update/end`.

Provider: `before_provider_headers`, `before_provider_request`, `after_provider_response`. Input: `input` (returns `{action:"continue"|"transform"|"handled"}`), `user_bash`. Context: `context` (mutate messages).

## 4. `ctx` (ExtensionContext)

`ctx.ui`: `notify(msg, type)`, `confirm`, `select`, `input`, `editor`, `custom` (TUI-only; returns `undefined` elsewhere), `setStatus(key, text)`, `setWidget`, `setTitle`, `onTerminalInput`.
`ctx.mode`: `"tui"|"rpc"|"json"|"print"`; `ctx.hasUI` (guard UI calls).
`ctx.cwd`, `ctx.model`, `ctx.thinkingLevel`, `ctx.isIdle()`, `ctx.abort()`, `ctx.signal`.
`ctx.sessionManager` (read-only): `getSessionId()`, `getSessionFile()`, `getName()`, `getHeader()`, `getBranch()`, `getEntries()`.
Command-only additions (`ExtensionCommandContext`): `waitForIdle`, `newSession`, `fork`, `switchSession`, `navigateTree`, `reload`.

## 5. Session lifecycle discipline (critical)

`session_shutdown` is **not** a process-exit event. It fires for `/new`, `/resume`,
`/fork`, `/reload` **and** `/quit`. Only `reason === "quit"` means the pi process is
going away; the pi process and its tmux pane survive the others.

- Never stop the daemon or kill tmux from `session_shutdown`.
- On non-quit reasons: release only per-session state; re-register on the next `session_start`.
- Captured `pi`/`ctx` become **stale** after a session replacement — post-replacement work goes in the `withSession` callback.

## 6. Environment

`PI_SESSION_ID`, `PI_SESSION_FILE`, `PI_PROVIDER`, `PI_MODEL`, `PI_REASONING_LEVEL`
are injected only into the **bash tool** env, not into extension code. Extensions
read equivalent values from `ctx.sessionManager` / `ctx.model`. `process.env` of
the pi process (e.g. `TMUX`, our `LDMUX_*`) *is* readable from extension code.

## 7. RPC / SDK (for the non-terminal seam)

- `pi --mode rpc`: strict **LF-only** JSONL on stdin/stdout; never use Node `readline` (it splits on U+2028/9). Commands carry optional `id`; responses are `{id, type:"response", command, success, data|error}`.
- SDK (in-process): `AgentSession` / `createAgentSession` from the package — used only if ldmux later needs a non-terminal attach path.

## 8. How ldmux uses this

`packages/extension/src/index.ts`:

- async-free factory; on `session_start` → `ensureDaemon()` then `register` the pi session id + cwd + `LDMUX_TMUX_NAME`;
- on `session_shutdown` → `session_end` **only when `reason === "quit"`**;
- `/ldmux` command + `ldmux` tool (list / snapshot / send) backed by `@ldmux/protocol`'s typed `daemonCall`.
- The daemon is a **separate detached process** (a pi extension cannot outlive a pi session); the extension only supervises its liveness.

## 9. Gotchas (from the ecosystem read)

- Tool names global-unique; first wins silently.
- `ctx.ui.custom()` is TUI-only; dialog methods degrade to no-ops in `json`/`print`.
- Long-lived resources belong to `session_start`/`session_shutdown` with reason-branching, not the factory.
- pi-research keeps a per-process service container guarded by `.isReady`; a rapid `/reload` can observe half-torn state — guard, don't assume.
- pi-fork ships its own `@sinclair/typebox`; extensions are not given pi's bundles. Prefer a plain JSON-Schema `parameters` object to avoid a dependency.