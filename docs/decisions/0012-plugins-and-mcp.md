# 0012 — Plugins as MCP servers behind the Security Policy Engine

- Status: Accepted
- Date: 2026-10-24

## Context

Phase 7 asks for a plugin API and MCP integration. The Model Context
Protocol (MCP) is the common way to give models tools: thousands of servers
exist (files, Git, browsers, issue trackers), written in any language and
started as child processes that speak JSON-RPC over stdio.

Plugins are third-party code and therefore untrusted. A plugin runs with the
user's rights, its tool descriptions are read by the agent's model (a prompt
injection channel), and its results are data written by someone else. The
existing guarantees must hold: every tool call goes through the Security
Policy Engine, high-risk actions ask the user, nothing unknown is started
without the user's consent, and secrets do not appear in files.

The protocol itself changed in the current revision **2026-07-28**: the
`initialize` handshake and `ping` are gone, every request carries the
protocol version and client capabilities in `_meta`, servers describe
themselves through `server/discover`, and tools may answer with
`resultType: "input_required"` (multi-round-trip requests). Many servers
still speak the older revisions (2025-11-25 and before) with `initialize`.

## Decision

- **A plugin is an MCP server.** The plugin API is: a folder with a
  `jarvis-plugin.json` manifest (format below) whose server JARVIS starts
  over stdio, or a plain MCP server entered as a command (program,
  arguments, working folder, environment) — the same shape other MCP
  clients use. No code is loaded into the JARVIS process: there is no
  native plugin ABI, so a plugin cannot reach JARVIS's memory, secrets or
  IPC.
- **Own minimal client in a new crate `jarvis-plugins`**, not the `rmcp`
  SDK: JARVIS needs only discovery, `tools/list` and `tools/call` over
  stdio, but needs tight control over what an untrusted process can do to
  it (message size limits, timeouts, cancellation, environment, no console
  window, process lifetime). The client:
  - probes with `server/discover` carrying its preferred modern version;
    a result or an `UnsupportedProtocolVersion` error (-32022) means a
    modern server; any other error or no answer within 10 s means a legacy
    server, and the client falls back to `initialize` +
    `notifications/initialized` (2025-11-25, accepting 2025-06-18,
    2025-03-26 and 2024-11-05). A slow server can answer either request:
    whichever succeeds first decides;
  - declares **no client capabilities** (no sampling, roots, elicitation
    or logging), answers legacy `ping` requests and refuses other server
    requests;
  - reads newline-delimited JSON with a size limit per message (8 MiB);
    a longer message or invalid UTF-8 ends the connection;
  - sends `notifications/cancelled` when a call is cancelled or times out
    and ignores late answers; an `input_required` result with only
    `requestState` is retried at most three times, one that asks for input
    fails with an explanation;
  - shuts down by closing stdin, waiting 2 s, then killing the process.
- **Tools join the existing registry.** Each enabled tool of a running
  plugin is registered as `<pluginId>__<tool>` (letters, digits, `_`, `-`;
  at most 64 characters) with a new capability `pluginTool`. The tool
  passes the input object to the server unchanged; the call's summary
  shows the plugin, the tool and the arguments. Built-in tools keep their
  settings; plugin tools are managed on the Plugins screen.
- **Risk is the user's decision, default high.** Every plugin tool starts
  at high risk, so each call asks for approval. The user may lower a tool
  to medium or low; then the global auto-approve switches apply. The
  lowered risk is bound to a hash of the tool's description and input
  schema: when the server changes the definition, the tool returns to high
  risk (protection against a tool that changes after being trusted). Tool
  annotations from the server (`readOnlyHint`, `destructiveHint`, …) are
  shown as hints only, as the specification requires for untrusted
  servers. Plugin tools are never treated as idempotent, so the agent never
  repeats a call by itself.
- **Consent before the first start, again after any change.** Adding a
  plugin shows the exact program (resolved path), arguments, working
  folder, environment variable names and secrets, and a fingerprint:
  SHA-256 over the launch settings, the program file, the manifest and
  every argument that names a file in the plugin folder. JARVIS stores the
  fingerprint the user approved and recomputes it before every start; any
  difference stops the plugin until the user reviews it again. Launchers
  that fetch code at run time (`npx`, `uvx`, `docker`, …) get a warning
  that JARVIS cannot check what they download.
- **Environment.** The server gets a small set of inherited variables
  (`PATH`, home and temp folders, user name, system folders — like the
  official SDKs), the plugin's own variables, and its secrets. Secrets
  (API keys) are entered on the Plugins screen and stored in the OS
  keychain (`plugin:<id>:<NAME>`), never in `plugins.json`.
- **Storage.** `<config>/plugins.json` holds the plugins, approvals and
  per-tool choices. It is written atomically and lives in the protected
  settings folder, so agent tools cannot change it. Removing a plugin
  deletes its entry and keychain secrets, never the plugin's folder.
- **Lifecycle.** Enabled, approved plugins start with JARVIS in the
  background. A server that exits unexpectedly is restarted at most three
  times (2 s, 10 s, 30 s; the count resets after 10 minutes of running);
  calls in flight fail and are not repeated. The Plugins screen shows the
  status, the server's name and version, its last stderr lines, and lets
  the user restart, refresh tools, disable or remove a plugin.
- **Prompt injection.** Plugin tool descriptions are clipped (600
  characters, one line) and listed in the agent prompt in a separate
  section that names the plugin and says the descriptions come from
  third-party plugins. Results are `<tool_result>` data like every other
  tool's output; images and binary resources are replaced by a short note.

### Manifest (`jarvis-plugin.json`, version 1)

```json
{
  "manifestVersion": 1,
  "id": "text-tools",
  "name": "Text tools",
  "version": "1.0.0",
  "description": "Counts words and converts text.",
  "author": "Example",
  "homepage": "https://example.com",
  "server": {
    "command": "node",
    "args": ["server.mjs"],
    "env": { "LOCALE": "en" },
    "secrets": [{ "name": "EXAMPLE_API_KEY", "description": "Key from example.com" }]
  }
}
```

`id` is 1–32 characters `a-z0-9-`; `command` is a file in the plugin folder
(a relative path that must stay inside it) or, when there is no such file, a
program found in `PATH` (`node`, `python`); the server runs in the plugin
folder. Unknown fields are refused.

## Consequences

- Any MCP server written for other clients works without changes, through
  both protocol generations.
- **No isolation is claimed.** A plugin process runs with the user's
  rights and can read files, use the network and start programs on its
  own. JARVIS controls only what the agent asks the plugin to do (policy,
  approvals, audit log) and what the plugin is started with. Sandboxing
  (AppContainer, bubblewrap/Landlock) is future work.
- On Windows, programs that the server itself starts are not stopped with
  it (no job object yet); on Unix the server gets `SIGKILL` without a
  `SIGTERM` first (no signal API without `unsafe` or another dependency).
- Not supported yet: HTTP transports (remote MCP servers), resources,
  prompts, `subscriptions/listen` for modern servers (their tool list is
  refreshed on start and on request), sampling and elicitation, and tools
  in plain chat (only agents use tools).
