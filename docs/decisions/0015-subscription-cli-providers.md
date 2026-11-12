# 0015 — ChatGPT and Claude subscriptions through the vendors' CLIs

- Status: Accepted
- Date: 2026-10-31

## Context

Local models are too slow on many laptops. Users who pay for ChatGPT or
Claude want to use those subscriptions in JARVIS without buying API access.
Neither vendor offers a public API for subscription accounts. Both ship an
official command-line agent that signs in with the subscription:
the OpenAI Codex CLI (`codex`) and Claude Code (`claude`). JARVIS must not
handle the subscription credentials, must not give a third-party agent its own
access to the computer, and must keep the cloud opt-in.

## Decision

- Two backends, `codex` and `claudeCode`, implemented by `CliProvider` in
  `jarvis-ai`. Each request starts the installed CLI once (`codex exec`,
  `claude -p`) and passes the conversation through stdin: JARVIS's system
  instructions and the history become one prompt, since a CLI run takes a
  single message. Claude Code reads it as one `--input-format stream-json`
  user message. That lets the process for the next request start right after
  a request with the same model and wait for it; it is stopped after
  5 minutes without a message, when another model is used, or when the
  provider is replaced or JARVIS closes. A waiting process holds about
  150 MB of memory (296 MB committed in the 2026-11-12 check).
- **Sign-in stays with the CLI.** JARVIS stores no key for these backends and
  never reads the CLI's credential files. Model listing runs `codex login
  status` / `claude auth status` and only looks at the exit code; a chat
  request does not (its model's capabilities come from the static list, and a
  signed-out CLI fails the request with the same hint). Signing in
  (`codex login`, `claude auth login`) is done by the user in a terminal.
- **The CLI's own agent is switched off** with its documented options, so the
  model only answers with text and JARVIS tools still go through the Security
  Policy Engine (text protocol):
  - Codex: `--sandbox read-only`, `--ignore-user-config`, `--ignore-rules`,
    `--ephemeral`, the tool and service features (`shell_tool`,
    `unified_exec`, apps, plugins, hooks, browser and computer use, image
    generation, sub-agents, memories, …) disabled with `-c features.*=false`
    and `web_search=disabled`.
  - Claude Code: `--safe-mode` (no CLAUDE.md, plugins, hooks, MCP servers or
    skills), `--tools ""`, `--disallowedTools "mcp__*"`,
    `--disable-slash-commands`, `--no-session-persistence`, `--max-turns 1`
    and a replaced system prompt. `--bare` is not used: it ignores the
    subscription sign-in. The process gets
    `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, which skips the update
    check, telemetry and a summary request after each answer.

  These are requests to a third-party program, not isolation enforced by
  JARVIS; a CLI update could change their effect.
- **Containment:** the CLI runs in a new empty temporary folder (the
  conversation's images are written there for Codex `--image` and deleted with
  it), as an owned process tree (`jarvis_platform::spawn_owned`: kill-on-close
  job on Windows, process group on Unix), without JARVIS's local-model key
  variables. Dropping the stream kills the tree; JSON lines are capped at
  8 MiB and only the last 8 KiB of stderr is kept for error messages.
- **Privacy:** the base URL is fixed (`https://chatgpt.com`,
  `https://claude.ai`) and validated, so the locality is always third-party
  cloud and the existing cloud, memory, capture and agent permissions apply.
- **Models:** Codex lists "default" plus the models its own cache
  (`$CODEX_HOME/models_cache.json`, not a public format) shows for the account;
  Claude Code lists "default" and the documented aliases `opus`, `sonnet`,
  `haiku` and `fable`. Model IDs are checked so they cannot become options.
  No native tool calling and no embeddings; Codex models accept images when
  the cache lists image input. All Claude Code models accept images (PNG and
  JPEG): they travel as base64 image blocks after the text of the
  `stream-json` message, named `image-1.png`, … in the transcript; no file is
  written. Images reach a cloud model only with the capture-sharing
  permission.
- **Errors:** usage limits become `AiError::LimitReached` (not retried),
  sign-in problems name the login command, a missing CLI explains how to
  install it.

## Consequences

- Subscription limits and the vendors' terms apply; JARVIS shows the CLI's
  own limit message.
- Each request pays the CLI's start-up time. Live check on 2026-11-12
  (Claude Code 2.1.316, Opus, a one-word reply): about 1.1 s to start and
  2.2 s to the first text and to the end; before the changes of that day the
  end came 1–1.7 s later, and JARVIS added about 3 s before starting the CLI
  while a local server was off (a sign-in check and a new model listing per
  message). With the waiting process, the next replies ended after
  1.3–1.4 s (first request 2.8 s). Codex gives its answer at the end of the turn: like
  `codex exec --output-last-message`, the last agent message is the answer
  and earlier ones ("I'll run …") are dropped. Claude Code streams text.
- Sampling options (temperature, output limit) are left to the CLI.
- The integration follows the CLIs' current flags and JSON events; tests use
  a fake CLI, so a changed CLI shows up only in live checks.
