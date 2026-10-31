# Native agent smoke test

For the embedded runtime, open **Runtime → Inference settings → Chat models
with native tools**, select a compatible model, save, then click **Restart** in
the running chat model's server panel. The panel must show **Native tools enabled**.
This setting is
opt-in per local model ID; embeddings are excluded. Clearing the selection
and restarting restores the text protocol.

Selected models launch with `--jinja`, as required by the
[llama.cpp function-calling documentation](https://github.com/ggml-org/llama.cpp/blob/master/docs/function-calling.md).
The model must include a tool-compatible template; enabling the setting does
not verify model quality. The managed runtime uses a private API key, so use
the application's agent UI to validate it rather than the unauthenticated
standalone probe below. Test a read-only task in an explicitly allowed folder,
then approval denial and cancellation. No live embedded-model pass is claimed
by the scripted lifecycle tests.

Start an on-device Ollama or OpenAI-compatible server with a tool-capable model
already installed. The probe does not download models, start servers, or change
the application's provider settings.

From the repository root:

```powershell
cargo run -p jarvis-agent --example native-agent-probe -- ollama http://127.0.0.1:11434 <model-id>
cargo run -p jarvis-agent --example native-agent-probe -- openai-compatible http://127.0.0.1:1234/v1 <model-id>
```

Replace `<model-id>` with the exact installed model ID. For an OpenAI-compatible
server this explicitly declares native-tool support for the selected model,
just like the Models screen's per-model setting. Ollama must report `tools`
capability itself. Remote URLs and URL credentials are rejected. This probe
does not use saved API keys.

The probe creates a temporary workspace containing `probe.txt` with a random
marker that is not included in the task prompt. It runs the real agent planner,
native executor and verifier through JARVIS's HTTP provider adapter. Only
`read_file` is enabled; access is scoped to the fixture directory, with no
program or network tools. The database and audit log are in memory. Temporary
files are removed on exit.

A passing run requires all of the following:

- A completed task and at least two native model turns.
- A successful `read_file` journal entry containing the random marker.
- A successful read in the audit log and intact audit integrity.
- A final answer containing the same marker.

Requests have a 60-second deadline. The task permits four steps and 180 seconds,
plus a bounded shutdown allowance. Exit codes: `0` for pass, `1` for a failed
probe, `2` for invalid arguments or configuration. A fluent answer without the
actual read is a failure.

Offline HTTP contract tests exercise both protocols and verify rejection of
a task that answers without reading:

```powershell
cargo test -p jarvis-agent --example native-agent-probe --locked --offline
```

The example's tests are also enabled in the ordinary `cargo test --workspace`
suite, so CI checks the HTTP-to-agent path without a model server installed.

Record the server version, model ID, command, date and probe output when doing
live validation. These automated tests are not evidence of a real model pass.
The probe covers the Rust pipeline, not the desktop UI or microphone.

## Core HTTP regression scenarios

`cargo test -p jarvis-core --test native_agent_http --locked --offline` runs
the real core, persisted provider settings, routing, HTTP adapters, scoped
file tools, approval broker, task journal and audit log against scripted
loopback servers. Both Ollama and OpenAI-compatible protocols cover:

- approval and denial, with no file created before approval;
- cancellation of an in-flight native HTTP request, including client disconnect;
- cancellation while awaiting approval, rejecting a late Allow;
- unadvertised tool calls, with no execution or silent text fallback;
- a verifier HTTP error, preserving the answer as `unverified`;
- reopening the core with its saved settings and completed journal, without
  replaying the tool.

These tests run in the workspace test suite. They do not launch a desktop
WebView or prove model/template compatibility. Live validation is still pending:
the standard loopback endpoints on ports 11434, 1234 and 8080 refused
connections during the 2026-10-29 check.
