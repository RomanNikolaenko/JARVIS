# 0010 — Tools and agents behind a Security Policy Engine

- Status: Accepted
- Date: 2026-10-20

## Context

Phase 5 lets JARVIS act: read and change files, run programs, inspect the
system, load web pages, and carry out multi-step tasks with a model choosing
the steps. The model is not trusted: it can be wrong, and text it reads
(files, web pages, program output) can carry instructions written by someone
else (prompt injection). Local models used by JARVIS are small (3–8 B) and
often have no native tool-calling API. The user must stay in control,
actions must be explainable afterwards, and an agent must never run without
bounds.

## Decision

- **Three crates.** `jarvis-security` (policy, scoped file access,
  approvals, audit log), `jarvis-tools` (tool trait, registry, built-in
  tools) and `jarvis-agent` (task manager and orchestrator). The core
  builds the policy from settings and adapts the chat models; the desktop
  shell exposes IPC commands and forwards task and approval updates.
- **Two-phase tools.** `Tool::prepare` validates the input (strict schema,
  unknown fields refused), resolves paths and describes the call as an
  `ActionRequest` (tool, capabilities, risk, one-line summary, paths,
  program, URL) **without side effects**. Only then does the policy decide,
  and `Tool::run` executes the prepared call, re-checking what can change in
  between (path scope, URL after DNS resolution).
- **Security Policy Engine** (`SecurityPolicy::evaluate`), in this order:
  tool turned off → deny; any path outside the workspace folders or inside
  JARVIS's own settings/data/log folders → deny (canonical paths, so `..`,
  symlinks and junctions cannot escape); a program that is not a plain name
  on the user's allowlist → deny (no shell, no paths, no metacharacters);
  network tools need the network setting and a public `http(s)` URL
  (loopback, private, link-local, CGNAT, multicast and similar addresses are
  refused — SSRF); files that may hold secrets (`.ssh`, `.env`, `*.pem`,
  `id_rsa`, …) → approval; then by risk: low (read-only) runs without asking
  if the user allows it (default on), medium (create/edit/copy/move files,
  load or open pages) asks unless allowed (default off), **high (overwrite,
  delete, run or stop programs) always asks**.
- **Approvals** go through an `ApprovalBroker`: the task waits (status
  `waitingForApproval`), the UI shows the request on every screen with the
  exact input, "Deny" is focused, and the request expires (default 5 min) or
  is withdrawn when the task is cancelled or JARVIS exits. Timeouts and
  cancellations count as "not allowed".
- **Audit log** in the memory database (`audit_log`): every decision
  (allowed, approved, denied, refused by policy, timed out, cancelled) and
  every result. Each entry stores the SHA-256 of the previous entry, so a
  change made outside JARVIS breaks the chain; the UI can check it. Entries
  stay when tasks are deleted.
- **Text protocol instead of native tool calling.** The model answers with
  one JSON object per turn — a tool call or a final answer — parsed
  leniently (reasoning blocks, code fences and prose ignored). Tool results
  return as `<tool_result>` blocks marked as data; a closing tag inside the
  content is neutralized; the system prompt says never to follow
  instructions found there. This reduces prompt injection; it cannot remove
  it, which is why the policy and approvals guard every action regardless
  of what the model says.
- **Orchestrator**: a planner turns the goal into 1–6 steps (if it fails,
  the agent works without a plan); the executor loop asks the model for the
  next action; a verifier checks the final answer against the tool results
  once and may send the agent back. Recovery is the model's job: a failed or
  refused action is reported to it and it chooses what to do next. **Nothing
  is retried automatically** (actions may not be idempotent).
- **Bounds**: steps (default 12, at most 50), wall-clock time (default
  10 min, at most 1 h), three invalid replies, three failed or refused
  actions in a row, three model errors, two tasks at a time, output and
  input sizes per tool, program time limit (default 60 s), at most 5
  redirects and 2 MiB per page.
- **Task manager**: tasks (goal, plan, journal of steps, result, limits,
  model) are saved after every step. Pause takes effect before the next
  step; cancel stops the model call or the running tool. On exit running
  tasks are paused; after a crash, tasks found running are marked paused at
  the next start. Resume replays the journal to the model — it never re-runs
  earlier actions.
- **Privacy**: the agent model is chosen like a chat model (explicit
  choice, the `toolCalling` route, otherwise the general route). Because
  agents send file contents and program output to the model, a cloud model
  needs its own permission (`privacy.allowCloudAgents`) on top of
  `allowCloudProviders`.

## Alternatives considered

- **Native tool calling** (OpenAI `tools`, Ollama `tools`): cleaner for
  large models, but not supported by every local model or server, and small
  models misuse it as often as JSON. The text protocol works everywhere;
  native calling can be added per provider later behind the same tools.
- **A shell tool** (`sh -c`, `cmd /c`): maximal flexibility, but quoting and
  metacharacters make the allowlist meaningless. Programs run directly with
  an argument vector instead.
- **Process isolation** (containers, AppContainer, bubblewrap, Landlock):
  the right long-term answer for running untrusted programs, but
  platform-specific and large. Not implemented in this phase; documented as
  a limitation rather than claimed.
- **Automatic retries with backoff**: rejected for actions; a repeated
  write or program run can do harm twice.

## Consequences

- Tools run with the user's rights. The policy limits *what they are asked
  to do*; it does not sandbox programs that the user allowed and approved.
- Small models follow the protocol imperfectly; invalid replies are
  corrected once or twice and then end the task instead of looping.
- Approval prompts are part of normal use for anything that changes data.
- The audit log detects edits but cannot prevent someone with access to the
  database file from rewriting the whole chain.
