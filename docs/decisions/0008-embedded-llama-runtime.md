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
- **No autostart by default.** A model starts automatically only if the user
  selects one in the runtime settings.

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
  real engine is verified manually.
- On forced termination of JARVIS (e.g. killed by the OS) the server may
  outlive it until the next start, which cleans it up.
