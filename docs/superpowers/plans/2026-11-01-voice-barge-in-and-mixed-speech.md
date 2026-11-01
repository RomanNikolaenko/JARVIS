# Voice barge-in and Ukrainian speech of foreign words — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user interrupt a spoken reply by talking (the phrase becomes the next voice request), and make Ukrainian voices read Latin-script words, numbers and symbols instead of dropping them.

**Architecture:** A pure text pass (`spoken`) runs before every Ukrainian synthesis. For barge-in, the speaker records its recent loudness in a shared `PlaybackMeter`; an `EchoGate` compares each microphone frame with it, and a `BargeInDetector` (framing, VAD scorer, audio history) reports the user's speech with its pre-roll. The engine opens a `BargeIn` listener while a reply to a voice request plays and, on detection, stops speaking and recognizes the phrase.

**Tech Stack:** Rust 2024 (`jarvis-voice`, `jarvis-core`), cpal, earshot VAD, Piper; Angular 22 (Settings → Voice); ts-rs bindings.

**Spec:** `docs/superpowers/specs/2026-11-01-voice-barge-in-and-mixed-speech-design.md`

## Global Constraints

- No new crate dependencies; no `unsafe` (workspace forbids it); no `unwrap()` in non-test code.
- Code, comments and repo docs in English; UI strings in English like the rest of Settings.
- The microphone opens only with a visible indicator (`VoiceState.mic_open`) and an event-log entry; `say()` / "listen to the voice" never opens it.
- Barge-in thresholds from the spec: reference = loudest playback in the last 300 ms; learn-only for the first 0.5 s of audible playback; candidate = VAD speech and RMS > `max(2 × c × reference, noise floor)`; noise floor = 2 × quietest recent frame RMS, at least 0.003; trigger = 12 of the last 16 frames; history 1.5 s; phrase starts 300 ms before the first candidate.
- `VoiceSettings.interrupt_by_voice` defaults to `true` (old settings files too).
- Before any push: full local CI in a clean worktree (`verify-ci.sh` in the session scratchpad), then GitHub CI green. Commit only this work: other sessions' uncommitted changes in `CHANGELOG.md`/`ARCHITECTURE.md` stay out (stage our hunks only).

## Review Focus

- The user turns the volume up mid-reply: the coupling must rise fast enough that JARVIS does not interrupt itself (Task 4 test `volume_up_mid_reply_does_not_trigger`).
- The user starts talking just as the reply ends: a barge-in detected before the speech finished must still produce a request (Task 7 test `barge_in_at_the_end_of_a_reply_still_listens`).
- Long digit runs (phone numbers, IDs) and versions like `1.2.3` must not become nonsense numbers (Task 1 test `long_digit_runs_and_versions`).
- Cyrillic capitals (`УВАГА`, `ЗСУ`) are not Latin acronyms and stay unchanged (Task 2 test `cyrillic_text_is_untouched`).
- A microphone that fails to open must not stop or loop the speech (Task 7 test `a_failing_microphone_does_not_stop_the_reply`).

---

### Task 1: Numbers and symbols for Ukrainian voices

**Files:**
- Create: `crates/jarvis-voice/src/spoken.rs`
- Modify: `crates/jarvis-voice/src/lib.rs` (module), `crates/jarvis-voice/src/engine.rs` (`synthesize`: apply for `SpeechLanguage::Uk`)

**Interfaces:**
- Produces: `pub fn for_ukrainian_voice(text: &str) -> String` in `spoken.rs` (Task 2 extends it); `pub(crate) fn number_words(digits: &str) -> String` (non-negative integer, nominative).

- [ ] **Step 1: Write the failing tests** (`spoken::tests`)

```rust
#[test]
fn numbers_become_words() {
    assert_eq!(for_ukrainian_voice("версія 3"), "версія три");
    assert_eq!(for_ukrainian_voice("2026 року"), "дві тисячі двадцять шість року");
    assert_eq!(for_ukrainian_voice("1 000 000 і 1,000,000"), "один мільйон і один мільйон");
    assert_eq!(for_ukrainian_voice("-5"), "мінус п'ять");
    assert_eq!(for_ukrainian_voice("3,5 і 3.5"), "три кома п'ять і три крапка п'ять");
    assert_eq!(for_ukrainian_voice("о 10:30"), "о десять тридцять");
    assert_eq!(for_ukrainian_voice("0"), "нуль");
    assert_eq!(for_ukrainian_voice("11 12 19 40 90 100 215 1001"), "одинадцять дванадцять дев'ятнадцять сорок дев'яносто сто двісті п'ятнадцять одна тисяча один");
}

#[test]
fn percentages_agree_with_the_number() {
    assert_eq!(for_ukrainian_voice("1%"), "один відсоток");
    assert_eq!(for_ukrainian_voice("3%"), "три відсотки");
    assert_eq!(for_ukrainian_voice("50 %"), "п'ятдесят відсотків");
    assert_eq!(for_ukrainian_voice("12%"), "дванадцять відсотків");
}

#[test]
fn long_digit_runs_and_versions() {
    assert_eq!(for_ukrainian_voice("1.2.3"), "один крапка два крапка три");
    assert_eq!(for_ukrainian_voice("0501234567"), "нуль п'ять нуль один два три чотири п'ять шість сім");
}

#[test]
fn symbols_and_apostrophes() {
    assert_eq!(for_ukrainian_voice("a+b"), /* letters handled in Task 2 */ for_ukrainian_voice("a плюс b"));
    assert_eq!(for_ukrainian_voice("20°"), "двадцять градусів");
    assert_eq!(for_ukrainian_voice("п’ять і пʼять"), "п'ять і п'ять");
    assert_eq!(for_ukrainian_voice("так/ні"), "так ні");
}

#[test]
fn ukrainian_text_without_digits_is_unchanged() {
    assert_eq!(for_ukrainian_voice("Привіт, як справи?"), "Привіт, як справи?");
}
```

Rule for long runs: a digit run of more than 9 digits without separators, or starting with `0` and longer than 1, is read digit by digit. Thousands groups: `\d{1,3}([ \u{a0},]\d{3})+` with no other separator. A dot between digit runs is «крапка», a comma «кома».

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p jarvis-voice --lib spoken`
Expected: FAIL (module missing).

- [ ] **Step 3: Implement `for_ukrainian_voice` and `number_words`**, then call `spoken::for_ukrainian_voice(text)` in `VoiceEngine::synthesize` when `language == SpeechLanguage::Uk`, before the synthesizer gets the text.

- [ ] **Step 4: Run tests**

Run: `cargo test -p jarvis-voice --lib spoken`
Expected: PASS.

- [ ] **Step 5: Commit** `feat(voice): read numbers and symbols with Ukrainian voices`

### Task 2: Latin-script words for Ukrainian voices

**Files:**
- Modify: `crates/jarvis-voice/src/spoken.rs` (the dictionary is a `const` table; move it to `crates/jarvis-voice/src/spoken_words.rs` if it passes ~150 lines)

**Interfaces:**
- Consumes: `for_ukrainian_voice` (Task 1).
- Produces: `fn latin_word(word: &str) -> String` (private), used by `for_ukrainian_voice` for every maximal run of Latin letters (after folding diacritics).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn dictionary_words() {
    assert_eq!(for_ukrainian_voice("Запусти Python і відкрий GitHub"), "Запусти пайтон і відкрий ґітхаб");
    assert_eq!(for_ukrainian_voice("Windows, Google, Claude, email"), "віндовс, ґуґл, клод, імейл");
}

#[test]
fn acronyms_are_spelled() {
    assert_eq!(for_ukrainian_voice("API"), "ей-пі-ай");
    assert_eq!(for_ukrainian_voice("USB і PDF"), "ю-ес-бі і пі-ді-еф");
    assert_eq!(for_ukrainian_voice("GPT-4"), "джі-пі-ті чотири");
    assert_eq!(for_ukrainian_voice("ChatGPT"), "чат джі-пі-ті");
}

#[test]
fn compounds_split_at_case_and_digits() {
    assert_eq!(for_ukrainian_voice("JavaScript"), "джава скрипт");
    assert_eq!(for_ukrainian_voice("v2"), "ві два");
}

#[test]
fn other_words_follow_spelling_rules() {
    assert_eq!(for_ukrainian_voice("update"), "апдейт");
    assert_eq!(for_ukrainian_voice("file"), "файл");
    assert_eq!(for_ukrainian_voice("cargo test"), "карґо тест");
    assert_eq!(for_ukrainian_voice("café"), "кафе");
}

#[test]
fn cyrillic_text_is_untouched() {
    assert_eq!(for_ukrainian_voice("УВАГА, ЗСУ!"), "УВАГА, ЗСУ!");
}

#[test]
fn the_voice_can_read_every_output_character() {
    // The symbol set of uk_UA-ukrainian_tts-medium.
    let readable = |c: char| c.is_whitespace() || "абвгґдеєжзиіїйклмнопрстуфхцчшщьюяАБВГҐДЕЄЖЗИІЇЙКЛМНОПРСТУФХЦЧШЩЬЮЯ'-,.!?:;—".contains(c);
    for text in ["Python 3.12, GitHub API, 50% & USB-C @home #1", "Ok, iPhone 15 Pro за $999"] {
        let out = for_ukrainian_voice(text);
        assert!(out.chars().all(readable), "{out}");
    }
}
```

Letter names for acronyms: A ей, B бі, C сі, D ді, E і, F еф, G джі, H ейч, I ай, J джей, K кей, L ел, M ем, N ен, O оу, P пі, Q к'ю, R ар, S ес, T ті, U ю, V ві, W дабл-ю, X екс, Y вай, Z зед; joined with `-`. Acronym = 2–6 letters, all capitals, or a single letter. `$` before a number becomes «доларів» after it. The dictionary holds at least: python, javascript, typescript, java, rust, git, github, gitlab, google, chrome, windows, linux, ubuntu, macos, iphone, android, apple, microsoft, openai, chatgpt, claude, anthropic, codex, youtube, telegram, instagram, facebook, email, online, offline, ok, wi-fi, docker, npm, pnpm, cargo, tauri, angular, react, node, ollama, whisper, piper, jarvis, hello, the, and, of, to, is.

- [ ] **Step 2: Run them to see them fail** — `cargo test -p jarvis-voice --lib spoken` → FAIL on the new tests.
- [ ] **Step 3: Implement** the dictionary, acronym spelling, compound split and the ordered spelling rules from the spec (digraphs before single letters; final silent `e` with long vowel; `c`/`g` softening; `y` as vowel; `g` → «ґ»).
- [ ] **Step 4: Run tests** → PASS. Then a live check (no microphone): pipe the Step 1 sentences through `for_ukrainian_voice` and `piper --debug` with the installed Ukrainian voice; expected: no `Missing "…" from phoneme/id map` warnings.
- [ ] **Step 5: Commit** `feat(voice): read Latin-script words with Ukrainian voices`

### Task 3: Playback meter

**Files:**
- Create: `crates/jarvis-voice/src/echo.rs` (meter part)
- Modify: `crates/jarvis-voice/src/backend.rs` (`VoiceBackend::open_output`), `crates/jarvis-voice/src/audio.rs` (`Player::open`, output callback), `crates/jarvis-voice/src/testing.rs` (`FakeSpeaker`), `crates/jarvis-voice/src/engine.rs` (`Inner.meter`, both `open_output` calls)

**Interfaces:**
- Produces:
  - `#[derive(Clone, Default, Debug)] pub struct PlaybackMeter` with `pub fn record(&self, samples: &[f32])` (RMS of the block, stamped `Instant::now()`, keeps the last 2 s), `pub fn loudest_within(&self, window: Duration) -> f32`, `pub fn clear(&self)`.
  - `fn open_output(&self, device: Option<&str>, meter: PlaybackMeter) -> Result<Box<dyn OutputDevice>>`.
  - `FakeSpeaker::enqueue` records the queued samples into the meter (tests treat queued as played).

- [ ] **Step 1: Failing tests** in `echo::tests`: `meter_reports_the_loudest_recent_block` (record `[0.1; 256]` then `[0.4; 256]` → `loudest_within(300ms) ≈ 0.4`; after `clear()` → `0.0`) and `old_blocks_expire` (record, sleep 350 ms → `loudest_within(300ms) == 0.0`).
- [ ] **Step 2: Run** `cargo test -p jarvis-voice --lib echo` → FAIL.
- [ ] **Step 3: Implement** the meter; in `build_output` record the samples actually written by each callback (mono values before channel fan-out); pass the engine's meter from `run_speech` and `chime`.
- [ ] **Step 4: Run** `cargo test -p jarvis-voice` → PASS (existing engine tests still pass).
- [ ] **Step 5: Commit** `feat(voice): record how loud the speaker plays`

### Task 4: Echo gate

**Files:**
- Modify: `crates/jarvis-voice/src/echo.rs`

**Interfaces:**
- Produces: `pub struct EchoGate`, `pub fn EchoGate::new() -> Self`, `pub fn push(&mut self, frame_rms: f32, speech: bool, reference: f32) -> Option<usize>` — returns `Some(frames_back)` on barge-in, where `frames_back` counts frames from the first candidate of the triggering window to the current frame (inclusive); `pub fn reset(&mut self)`.

- [ ] **Step 1: Failing tests** with a helper that simulates frames (`reference` from a speech-like envelope, `echo = gain × reference(t − delay) + noise`):
  - `echo_alone_never_triggers` (gains 0.05, 0.5, 2.0; 30 s each; VAD = true whenever echo is audible).
  - `volume_up_mid_reply_does_not_trigger` (gain doubles after 3 s).
  - `speech_over_the_echo_triggers` (user frames at 3 × the echo from second 4 → `Some` within 19 frames of the onset, `frames_back` between 12 and 16).
  - `speech_in_a_pause_triggers` (reference 0 for the user's frames; VAD speech; triggers after 12 frames).
  - `headphones_trigger_on_any_speech` (gain 0.0).
  - `nothing_triggers_while_learning` (loud user speech during the first 31 audible frames → `None`; triggers afterwards).
- [ ] **Step 2: Run** `cargo test -p jarvis-voice --lib echo` → FAIL.
- [ ] **Step 3: Implement** per the Global Constraints values; coupling tracks the upper envelope of `frame_rms / reference` on non-candidate audible frames: `c += 0.2 × (r − c)` when `r > c`, `c -= 0.01 × (c − r)` otherwise.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(voice): tell the user's voice from JARVIS's echo`

### Task 5: Barge-in detector

**Files:**
- Create: `crates/jarvis-voice/src/barge_in.rs`
- Modify: `crates/jarvis-voice/src/lib.rs`

**Interfaces:**
- Consumes: `EchoGate`, `PlaybackMeter`, `SpeechScorer`, `vad::{FRAME, SAMPLE_RATE}`, `SegmenterConfig::default().threshold`.
- Produces: `pub struct BargeInDetector`; `pub fn new(scorer: Box<dyn SpeechScorer>, meter: PlaybackMeter) -> Self`; `pub fn push(&mut self, samples: &[f32]) -> Option<Vec<f32>>` — frames the 16 kHz audio, keeps 1.5 s of history, and on barge-in returns the audio from 300 ms before the first candidate frame up to now.

- [ ] **Step 1: Failing tests** (with `testing::LoudIsSpeech` and a meter): `returns_the_phrase_from_its_start` (silence 1 s, then 0.5 amplitude → returned audio starts within the 300 ms pre-roll of the onset and is shorter than the history), `quiet_echo_is_ignored` (meter at 0.4 recording each 16 ms, mic at 0.2 → no trigger after learning).
- [ ] **Step 2: Run** `cargo test -p jarvis-voice --lib barge_in` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(voice): detect speech over a spoken reply`

### Task 6: Setting, transcript source and state

**Files:**
- Modify: `crates/jarvis-voice/src/types.rs` (`VoiceSettings.interrupt_by_voice: bool` default `true`; `TranscriptSource::BargeIn`; `VoiceState.interrupt_listening: bool`), `crates/jarvis-core/src/voice.rs` (event message), `apps/desktop/src/app/features/voice/voice.{ts,html,spec.ts}`, `packages/shared-types/src/generated/voice.ts` (regenerated)

**Interfaces:**
- Produces: `settings.interrupt_by_voice`; `TranscriptSource::BargeIn` (`"bargeIn"`); `VoiceState::interrupt_listening` (`interruptListening`), true while a `BargeIn` listener waits for speech.

- [ ] **Step 1: Failing tests:** in `types.rs` `old_settings_allow_interrupting` (deserialize `{}` → `interrupt_by_voice == true`); in `voice.spec.ts` the form shows a checkbox `formControlName="interruptByVoice"` with the text "Interrupt JARVIS by speaking" and saving sends `interruptByVoice: false` after unchecking.
- [ ] **Step 2: Run** `cargo test -p jarvis-voice --lib types` and `npx --no-install ng test --watch=false --include src/app/features/voice/voice.spec.ts` (in `apps/desktop`) → FAIL.
- [ ] **Step 3: Implement**; hint text: "The microphone stays on while JARVIS reads a reply to a spoken request, so you can talk over it. Works best with headphones; on laptop speakers speak a little louder than JARVIS." Core event message when `mic_open && interrupt_listening`: "Microphone on: JARVIS can be interrupted by voice". Regenerate bindings (`cargo test -p jarvis-desktop --lib export_typescript_bindings` in the clean worktree, or edit the generated union/fields to match ts-rs output and let the CI gate confirm).
- [ ] **Step 4: Run** the tests above → PASS.
- [ ] **Step 5: Commit** `feat(voice): setting and state for interrupting by voice`

### Task 7: Engine integration

**Files:**
- Modify: `crates/jarvis-voice/src/engine.rs`, `crates/jarvis-core/src/memory.rs:451`
- Test: `crates/jarvis-voice/tests/engine.rs`

**Interfaces:**
- Consumes: Tasks 3–6.
- Produces: `pub fn speak_reply(&self) -> SpeechStream` (like `speak()`, plus barge-in); `ListenMode::BargeIn`; the wake-word command slot remembers its `TranscriptSource`.

Behaviour (spec §2): the `BargeIn` listener starts when the first sentence of a `speak_reply` stream starts playing, if `interrupt_by_voice` and no listener runs; it ends when the speech ends unless barge-in already happened. On barge-in: `stop_speaking()`, new `capture_id`, phase `Listening`, a fresh `Segmenter` fed with the detector's audio, then like push-to-talk; emitted source `BargeIn`. In wake word mode, while speaking: phrases are not sent to the wake worker; on barge-in the segmenter is re-seeded and `expect_command` arms with source `BargeIn`.

- [ ] **Step 1: Failing tests** (`tests/engine.rs`, fake backend):
  - `speaking_a_reply_opens_the_microphone_for_interruptions` (`speak_reply` + `hold_playback` → `mic_open && interrupt_listening`; with the setting off → microphone stays closed).
  - `say_never_opens_the_microphone`.
  - `talking_over_a_reply_stops_it_and_becomes_a_request` (`speak_into_mic(1.0)` → `cleared >= 1`, speech stopped, one transcript with `source == BargeIn`).
  - `barge_in_at_the_end_of_a_reply_still_listens` (playback ends right after the trigger → transcript still emitted).
  - `a_failing_microphone_does_not_stop_the_reply` (fake `open_input` error → all sentences played, `last_error` set once).
  - `wake_word_mode_takes_the_interruption_as_a_command`.
- [ ] **Step 2: Run** `cargo test -p jarvis-voice --test engine` → FAIL.
- [ ] **Step 3: Implement**; `jarvis-core` uses `speak_reply()` for voice turns.
- [ ] **Step 4: Run** `cargo test -p jarvis-voice -p jarvis-core` → PASS.
- [ ] **Step 5: Commit** `feat(voice): interrupt a spoken reply by talking`

### Task 8: Docs, verification, live check, push

**Files:** `docs/decisions/0011-voice-and-vision.md`, `SECURITY.md`, `CHANGELOG.md` (our hunks only), `ROADMAP.md`, `ARCHITECTURE.md` if it lists voice modes (our hunks only).

- [ ] **Step 1:** Update the docs (barge-in by voice with the echo gate and its limits; text for Ukrainian voices; microphone during spoken replies; echo cancellation stays planned).
- [ ] **Step 2:** Full local CI in a clean worktree at the final commit: `bash <scratchpad>/verify-ci.sh D:/Works/jarvis-verify` → every gate `ok` (a Defender block of the runtime test binary is rerun once).
- [ ] **Step 3:** Ask the user for consent, then the live check on their laptop: a long Ukrainian reply with English words and numbers through the speakers (no self-interruption, words and numbers heard), then the user talks over it (it stops; the new request is answered); also with headphones if available.
- [ ] **Step 4:** Push to `main`, wait for GitHub CI green.
