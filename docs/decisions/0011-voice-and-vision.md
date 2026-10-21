# 0011 — Voice and vision on the user's computer

- Status: Accepted
- Date: 2026-10-21

## Context

Phase 6 lets the user talk to JARVIS, hear its replies, and show it the
screen, a window, an image or a camera photo. Speech and screenshots are the
most private data JARVIS handles: a microphone that opens on its own or a
screenshot sent to a cloud model would break the local-first promise. The
assistant must speak Ukrainian and English, run on Windows and Linux (X11
and Wayland) without a GPU, and must not claim abilities a system lacks.
The workspace forbids `unsafe` code, the dev machine has no CMake, and new
dependencies must earn their place.

## Decision

### Voice (`jarvis-voice`)

- **Independent parts** behind small traits: audio I/O (`cpal`: WASAPI,
  ALSA), voice activity detection (`earshot`, pure Rust, 16 ms frames),
  speech recognition, speech synthesis, the wake word, and a sentence
  splitter for streamed replies. `VoiceEngine` joins them; a `VoiceBackend`
  trait lets tests replace devices and engines.
- **Speech recognition: whisper.cpp** as a managed `whisper-server`
  process (the official build `b5454`), started on the first request and
  reused. The server has no authentication and answers any web origin, so
  it listens on `127.0.0.1` on a random port and every route lives under a
  random 128-bit path prefix; its static folder is an empty directory.
  Models: Whisper Base, Small (recommended for Ukrainian) and Large v3
  Turbo from `ggerganov/whisper.cpp` at a pinned commit.
- **Speech synthesis: Piper** (`rhasspy/piper` 2023.11.14-2) as one
  long-running process per voice: it reads JSON requests from stdin (UTF-8)
  and writes one WAV per sentence. sherpa-onnx was the first choice, but
  its command line takes the text through the ANSI code page on Windows,
  which garbles Ukrainian. Voices: Ukrainian (Lada/Mykyta/Tetiana; Lada
  compact) and English (Alan, Amy) from `rhasspy/piper-voices` at a pinned
  commit. Engine paths are passed relative to the voice folder, because
  both engines read their arguments in the ANSI code page on Windows.
- **Verified packages.** Every engine, model and voice is pinned to a
  release tag or commit with its SHA-256 and size (GitHub's digest, Hugging
  Face's LFS hash, or — for the 2023 Piper archives without one — a hash
  recorded independently and checked on download). The UI names size,
  source and license, and nothing is downloaded before the user confirms.
  Engines are extracted with the runtime's safe extractor; the SHA-256 of
  each program and its libraries is recorded and checked before every
  start.
- **The microphone opens only on request**: push-to-talk (the chat's
  microphone button) or wake word mode, which the user turns on in
  Settings → Voice. `VoiceState.micOpen` is true exactly while audio is
  captured; the UI shows a red indicator on every screen, and every opening
  and closing is written to the event log. Audio is processed in memory and
  never saved.
- **Wake word «Джарвіс» / "Jarvis"** is recognized by Whisper on phrases cut
  by the VAD, matched with a small edit distance near the start of the
  phrase (and requiring the "dzh"/"j" sound, so "Дарвін" or "Davis" do not
  match). Long phrases are checked on their first 3 seconds before being
  recognized in full. "Джарвіс" alone opens a 10-second window for the
  command (with a short chime).
- **Speaking replies**: chat turns made by voice set `speak`; the core
  feeds the streamed reply to a sentence splitter (Markdown, code blocks,
  links and reasoning blocks are not read) and synthesizes the next
  sentence while the previous one plays. The voice follows the text's
  language. **Barge-in**: the wake word, the microphone button or the stop
  button cut the speech and drop queued audio.

### Vision (`jarvis-vision`)

- **Capture backends**: Windows via `xcap` (displays and windows); X11 via
  `x11rb` (pure Rust: `GetImage`, RandR monitors, EWMH window list);
  Wayland via the xdg-desktop-portal Screenshot interface (`ashpd`), where
  the desktop shows its own dialog and the user picks a window or area
  there. Each backend reports `CaptureCapabilities` (available, system
  dialog, unavailable, with notes), so the UI offers only what works and
  says why. No capture happens without a user action; there is no agent
  tool for screenshots.
- **Images for models**: decoded with size limits (PNG and JPEG only),
  downscaled to at most 1568 px, encoded as JPEG, plus a 320 px preview the
  user sees before sending. Captures live in memory for at most 30 minutes
  and are used once; saved conversations keep a note
  (`[Attached screenshot: Display 1, 1568×882]`), not the image.
- **Vision models**: `ChatMessage.images` (never part of the IPC contract)
  become OpenAI-compatible `image_url` parts or Ollama `images`. A message
  with images uses the `vision` route, or the chosen model if it supports
  vision. Locally, llama-server loads a projector (`--mmproj`); the catalog
  offers Qwen3-VL 2B and Gemma 3 4B with their projectors, and any chat
  model can be given a projector file. **Cloud models receive images only
  with `privacy.shareCapturesWithCloud`**, on top of `allowCloudProviders`.
- **Camera**: the WebView's `getUserMedia` (the browser and the system ask
  for permission and show their indicators); the stream runs only while
  the photo dialog is open. Where the WebView offers no camera (WebKitGTK
  without media capture), the UI says so instead of failing silently.

## Alternatives considered

- **whisper-rs / sherpa-onnx linked into the app**: faster startup, but
  needs CMake and C++ toolchains on every build machine and FFI (`unsafe`).
  Managed processes keep the app safe-Rust and let users update engines
  independently.
- **Cloud speech** (OpenAI, Google): good quality, but sends raw audio
  away. Not implemented; it can be added later behind an explicit setting.
- **A dedicated wake-word model** (Porcupine, openWakeWord): cheaper than
  Whisper on every phrase, but Porcupine is proprietary and openWakeWord
  has no Ukrainian «Джарвіс» model. The Whisper-based check needs no
  training and works for both languages; its CPU cost is documented.
- **`xcap` on Linux**: pulls PipeWire bindings that need libclang at build
  time. The pure-Rust X11 and portal backends avoid that.

## Consequences

- Voice needs about 30 MB of engines plus a Whisper model (60–570 MB) and
  voices (20–80 MB). Wake word mode keeps one CPU core busy while people
  speak near the microphone.
- No echo cancellation: JARVIS may hear itself while speaking; a phrase must
  start with the name to count, which keeps false starts rare.
- X11 lets any program read the screen; JARVIS relies on its own UI to
  capture only on request. Wayland portals may keep a copy of each
  screenshot in the user's Pictures folder; JARVIS reads it but does not
  delete it.
- Images take context: small local models with 4 k contexts fit one or two.
