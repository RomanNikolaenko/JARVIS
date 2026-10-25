# Text tools — an example JARVIS plugin

A plugin is an [MCP](https://modelcontextprotocol.io) server with a
`jarvis-plugin.json` manifest (see
[ADR 0012](../../../docs/decisions/0012-plugins-and-mcp.md)). This one needs
only Node.js 18 or newer and offers two tools:

- `word_count` — words, characters and lines of a text;
- `slugify` — a lowercase, dash-separated slug.

## Try it

1. Open **Plugins** in JARVIS and choose **Choose a plugin folder…**.
2. Pick this folder. JARVIS shows what it will run (`node server.mjs`), the
   working folder and a fingerprint of the files.
3. Confirm with **Add and start**. The tools appear as
   `text-tools__word_count` and `text-tools__slugify`; agents can use them,
   and each call asks you first until you lower a tool's risk.

Changing `server.mjs` or the manifest changes the fingerprint: JARVIS then
stops the plugin until you review it again.

## Check a server

```bash
cargo run -p jarvis-plugins --example mcp-probe -- --call word_count '{"text":"a b c"}' -- node examples/plugins/text-tools/server.mjs
```

## Write your own

- Read newline-delimited JSON-RPC from stdin and write answers to stdout;
  write logs to stderr only.
- Answer `server/discover` (protocol 2026-07-28) and, for older clients,
  `initialize`; then `tools/list` and `tools/call`.
- Keep tool names to letters, digits, `_` and `-`, and describe each tool
  in one or two sentences.
- Secrets (API keys) go into `server.secrets` in the manifest; the user
  enters them on the Plugins screen and JARVIS passes them as environment
  variables from the OS keychain.
