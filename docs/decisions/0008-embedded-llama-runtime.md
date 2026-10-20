# 0008 — Embedded runtime: supervised llama.cpp server

- Status: Accepted
- Date: 2026-10-19

## Context

JARVIS must run local models without the user installing Ollama or LM Studio.
Inference engines are large native code bases with frequent releases and many
GPU backends. Running unknown executables or model files is a security risk,
and a stuck or crashed engine must never take the UI down.

## Decision

- **Engine as a child process.** JARVIS supervises the official llama.cpp
  `llama-server` (crate `jarvis-runtime`) instead of linking an inference
  library. The server is exposed to the rest of JARVIS as an
  OpenAI-compatible `AiProvider` (`jarvis-runtime` id, kind `embedded`,
  locality `onDevice`), so chat and routing need no special cases. An
  in-process engine can replace it later behind the same provider.
- **Installation only with explicit approval.** The UI shows the exact
  release (tag, asset name, size, SHA-256, URL) from
  `github.com/ggml-org/llama.cpp`. The core re-fetches that release by tag
  and refuses if the asset or digest changed, downloads it, verifies the
  SHA-256 that GitHub publishes for the asset, extracts it without following
  paths outside the target directory (zip-slip, absolute paths, escaping
  symlinks are rejected), and runs `llama-server --version` once. Builds:
  CPU and Vulkan (Windows `.zip`, Linux `.tar.gz`, x64/arm64). Other backends
  (CUDA, ROCm, SYCL) can be used by selecting an existing `llama-server`.
- **Tamper check.** The SHA-256 of the executable and of every shared library
  in its directory (`.dll`, `.so`, `.dylib`) is recorded at install and
  verified before every start; an added, removed or changed library blocks
  the start. Windows builds ship `llama-server.exe` as a 9 KB launcher with
  the server in `llama-server-impl.dll`, so hashing the executable alone
  would not protect anything.
- **Launch hardening.** The server listens on `127.0.0.1` only, on a free
  port, with a random per-launch API key, so other local programs and web
  pages in the user's browser cannot use it. The key is passed in the
  environment (`LLAMA_API_KEY`), not on the command line, which other users
  can read (`/proc/<pid>/cmdline`). After start-up JARVIS verifies
  that an unauthenticated request is rejected and logs a security event if
  not. Only long-standing command-line options are used. On Windows the
  console window is suppressed.
- **Supervision.** Readiness is polled on `/health` (503 while loading) with
  a configurable timeout; an early exit reports the last log lines; a crash
  after start-up unregisters the provider and marks the runtime failed.
  Stopping sends SIGTERM where the OS supports it, then kills. `kill_on_drop`
  prevents leaks if JARVIS drops the handle, and a pid file lets the next
  session stop a server left by a crash — only if the pid still runs the
  recorded executable.
- **Resource control.** Before start, JARVIS estimates memory (weights + f16
  KV cache for the configured context + overhead) from the GGUF header and
  refuses to start if it does not fit into available RAM (dedicated VRAM of
  ≥ 4 GiB is credited when layers are offloaded). GPU layers are automatic
  (all if the model fits in VRAM, a proportional share otherwise) or set by
  the user. Threads default to physical cores.
- **Backend choice.** The Vulkan build is recommended only when a GPU with at
  least 4 GiB of dedicated memory is detected. Live testing on a Radeon Vega
  8 (integrated, 2 GB) with llama.cpp b11739 gave garbled output with any
  number of offloaded layers, while the same model on the CPU answered
  correctly; integrated GPUs also share memory bandwidth with the CPU.
- **Models.** GGUF headers are parsed with strict bounds before a file is
  accepted. Files imported from elsewhere stay in place and are never deleted
  by JARVIS; it deletes only files inside its models folder, comparing
  canonical paths (the data folder may be a link or a redirected AppData).
  Downloads come only from Hugging Face over HTTPS, pinned to a commit and
  verified against the LFS SHA-256 Hugging Face publishes; a small curated
  catalog pins commit and checksum in source code. Downloads resume with HTTP
  ranges, check free disk space and can be cancelled.
- **A second server for embeddings** (added 2026-10-20). Semantic memory
  search needs an embedding model, and a chat server cannot serve one at the
  same time, so the `RuntimeManager` supervises one server per
  `ServerRole` (`chat`, `embeddings`). Each role is a `Slot` with its own
  state, provider, log tail, supervisor and pid file (`server.pid`,
  `embedding-server.pid`); both get the same hardening (verified binary and
  libraries, `127.0.0.1`, per-launch key in `LLAMA_API_KEY`, readiness,
  crash detection, orphan cleanup) because the code is shared, not copied.
  The embedding server runs with `--embedding`, `--pooling` from the model's
  GGUF header (`<arch>.pooling_type`; rerankers are refused, models without
  the key get `mean` with a warning) and context = batch = micro-batch =
  min(2 048, training context): llama-server rejects an input that does not
  fit into one micro-batch, and JARVIS embeds at most 2 000 characters per
  text. Its provider (`jarvis-runtime-embeddings`, `onDevice`) reports
  `embeddings` only, the chat provider `chat` + `streaming` only, so routing
  never mixes them up. Both servers may run side by side; a crash of one
  leaves the other running. The curated catalog offers BGE-M3 (Q8_0, from
  `ggml-org/bge-m3-Q8_0-GGUF`, pinned to a commit and the LFS SHA-256).
- **No autostart by default.** A model starts automatically only if the user
  selects one in the runtime settings (one setting per role; the embedding
  model starts first because it loads in seconds and the chat model's
  memory check then accounts for it).

## Alternatives considered

- Linking llama.cpp (`llama-cpp-2` or similar bindings): faster IPC and no
  extra process, but a crash or a GPU driver fault would kill JARVIS, builds
  need CMake and GPU SDKs on every developer machine, and backend selection
  would be a compile-time decision.
- Bundling llama.cpp in the installer: larger installers and a release cycle
  tied to llama.cpp's; can be added in Phase 8 on top of the same supervisor.
- Downloading without a published digest: rejected; integrity must not rely
  on TLS alone.

## Consequences

- The first use requires one explicit download (≈20–35 MB for CPU/Vulkan).
- Tests use a fake `llama-server` (the test binary re-executes itself), so
  the supervisor is tested with real processes but without llama.cpp; the
  real engine is verified manually. The fake also serves `/v1/embeddings`
  with deterministic vectors (words of one meaning share a dimension), so
  the core can test that search becomes semantic while the embedding server
  runs.
- Running both servers costs memory for two models (BGE-M3 Q8_0: ~0.6 GB
  of weights, ~870 MB resident on the CPU with a 2 048-token context;
  JARVIS requires ~1.2 GB free before it starts it).
- Live check (Windows, llama.cpp b11739, CPU): BGE-M3 loads in ~5 s; the
  right Ukrainian fact ranks first for queries without a shared word, also
  for English queries; a chat turn with Qwen2.5 3B received only the
  relevant fact. Unrelated facts passed the search similarity threshold
  (0.35) and appeared lower in the list; the similarity cuts were then
  measured and calibrated for BGE-M3 (ADR 0009).
- On forced termination of JARVIS (e.g. killed by the OS) the server may
  outlive it until the next start, which cleans it up.
