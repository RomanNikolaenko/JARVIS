# 0013 — All data stays on the user's computer; no cloud sync

- Status: Accepted
- Date: 2026-10-24

## Context

The original plan had an optional Supabase component (Phase 7): sign-in,
synchronization of settings, conversations and memory between devices, and
cloud backup, with an offline queue, conflict resolution and end-to-end
encryption. JARVIS already works fully without it: settings, conversations,
memory, agent tasks and the audit log live in local files and a local SQLite
database, and secrets in the OS keychain.

The owner decided not to connect Supabase: all data stays local.

## Decision

- No Supabase (or other cloud) sign-in, sync or backup. The `jarvis-sync`
  crate is not created.
- Nothing JARVIS stores leaves the computer except what the user sends to
  a cloud AI provider they configured, under the existing privacy rules.
- Phase 7 is reduced to extensions: the plugin API and MCP integration
  ([ADR 0012](0012-plugins-and-mcp.md)).

## Consequences

- One device per installation: moving to another computer means copying the
  data folders by hand (with JARVIS closed). A local export/import or an
  encrypted local backup can be added later without any server.
- No accounts, no server costs, no server-side secrets, and no new attack
  surface on the network.
- If sync is ever wanted again, it needs a new ADR (provider, encryption,
  conflict handling).
