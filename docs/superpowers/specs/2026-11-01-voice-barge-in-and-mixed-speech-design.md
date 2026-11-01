# Voice: interrupting JARVIS by speaking, and reading foreign words and numbers

- Date: 2026-11-01
- Status: approved design (implementation pending)
- Area: `crates/jarvis-voice`, `crates/jarvis-core` (voice turns), Settings → Voice

## Goals

1. **Barge-in by voice.** While JARVIS reads a reply to a voice request
   aloud, the user can start talking: JARVIS stops at once, hears the user's
   phrase to its end and answers it as a new voice request ("Stop, tell me
   about … instead").
2. **Ukrainian speech keeps every word.** Replies spoken with a Ukrainian
   voice read foreign (Latin-script) words, numbers and common symbols
   instead of dropping them.

Non-goals: full acoustic echo cancellation, case agreement of Ukrainian
numerals, foreign words in English speech, Cyrillic in English speech.

## Findings

- The Ukrainian Piper voice `uk_UA-ukrainian_tts-medium` is a grapheme
  model (`"phoneme_type": "text"`): its symbol map holds Cyrillic letters,
  stress and a few punctuation marks only. Piper drops every character that
  is not in the map, so "Запусти Python" is spoken as "Запусти" and "версія
  3" as "версія" (`Missing "p" …` warnings in `piper --debug`). Digits, `%`,
  `+`, `&`, `@` and the typographic apostrophes `’ ʼ` are missing too.
- Today speech is interrupted only by the wake word (wake word mode), the
  microphone button or the stop button. With push-to-talk the microphone is
  closed while JARVIS speaks. ADR 0011 records that there is no echo
  cancellation: an open microphone hears JARVIS's own voice.

## Design

### 1. Text for Ukrainian voices (`jarvis-voice`, new module `spoken`)

`fn for_ukrainian_voice(text: &str) -> String` runs on every sentence
spoken in Ukrainian (all Ukrainian voices, before `Synthesizer::synthesize`).
It is pure, deterministic and has no dependencies.

- **Latin-script words → Cyrillic as Ukrainians pronounce them.**
  1. A dictionary of frequent words, brands and tech terms (case-insensitive):
     Python «пайтон», GitHub «ґітхаб», Windows «віндовс», Google «ґуґл»,
     Claude «клод», ChatGPT «чат джі-пі-ті», email «імейл», JARVIS «джарвіс» …
  2. Acronyms (2–6 capitals, optionally with digits or a hyphen: API, USB,
     GPT-4) are spelled with English letter names in Cyrillic
     (A «ей», B «бі», G «джі», H «ейч», P «пі», T «ті», W «дабл-ю», Z «зед» …).
  3. Compound words split at case and digit boundaries (JavaScript → Java +
     Script, Qwen2 → Qwen + 2) and each part is converted.
  4. Other words: ordered English spelling rules (digraphs such as `sh`,
     `ch`, `th`, `ph`, `ck`, `qu`, `tion`, `ee`, `oo`, `ou`, `ow`, `ai`, `igh`;
     final silent `e` with a long vowel before it; `c`/`g` before `e i y`;
     `y` as a vowel; `x` → «кс»; `g` → «ґ»). The result is approximate; the
     dictionary covers words the rules get wrong.
  Letters with diacritics are folded to ASCII first (é → e).
- **Numbers → Ukrainian words** (nominative cardinals): integers with
  optional thousands separators (`1 000 000`, `1,000,000`), a minus sign,
  decimals (`3,5` «три кома п'ять»; `3.5` «три крапка п'ять», since a dot
  usually separates version parts), times `10:30` «десять тридцять»,
  percentages with the plural of «відсоток» (1 відсоток, 2–4 відсотки,
  5+ відсотків). Digits glued to words are separated (`v2` «ві два»).
- **Symbols → words or spaces**: `+` «плюс», `=` «дорівнює», `&` «і»,
  `@` «ет», `°` «градусів», `#` «номер», `/` and `_` become spaces; `’ ʼ`
  become `'`. Characters the voice cannot read are not replaced: Piper skips
  them as before.

English replies are unchanged: the English voices use espeak phonemes,
which read Latin script and numbers.

### 2. Barge-in by voice

**Setting.** `VoiceSettings::interrupt_by_voice: bool`, default `true`,
shown in Settings → Voice: "Interrupt JARVIS by speaking — the microphone
stays on while JARVIS reads a reply to a voice request". It applies only to
replies to voice requests (`speak` turns); the "listen to the voice" button
and other `say` calls never open the microphone.

**Microphone.** When a voice reply starts speaking and no listener runs, the
engine starts a listener in the new mode `ListenMode::BargeIn`; it ends when
the speech ends or is stopped. `VoiceState::mic_open` is true while it runs
(red indicator on every screen), and opening/closing is written to the
event log as for the other modes. In wake word mode the existing listener
does the same detection while JARVIS speaks.

**Playback meter.** The speaker records how loud it just played: the output
callback (and the fake speaker in tests) appends the RMS of each ~16 ms
block with its time to a shared `PlaybackMeter` (last ~2 s). The engine owns
one meter; `VoiceBackend::open_output` receives it.

**Echo gate** (new module `echo`, pure Rust, no dependencies). For every
16 ms microphone frame while JARVIS speaks:

- `reference` = the loudest played block in the last 300 ms (covers output
  and input latency and the room).
- The gate learns the coupling `c` = microphone RMS ÷ reference: during the
  first 0.5 s of audible playback only learning happens (no barge-in), and
  afterwards `c` keeps tracking the upper envelope of the ratio on frames
  that are not speech candidates (rises fast, decays slowly).
- A frame is a **speech candidate** when the VAD scores it as speech and its
  RMS is above `max(2 × c × reference, noise floor)`, where the noise floor
  is twice the quietest frame RMS seen recently (at least 0.003); when
  nothing is playing (pauses between sentences) the VAD alone decides.
- **Barge-in** when at least 12 of the last 16 frames (~200 of 256 ms) are
  candidates.

With headphones `c` is tiny and any speech interrupts; on laptop speakers
the user has to speak somewhat louder than JARVIS or in its pauses.
Self-interruption needs sustained energy well above the learned echo, which
JARVIS's own voice does not produce.

**After barge-in.**

1. `stop_speaking()`: queued audio is dropped and the speaker stops.
2. The listener keeps the last ~1.5 s of microphone audio; a fresh segmenter
   is fed from 300 ms before the first candidate frame, then with live audio,
   so the phrase starts where the user started.
3. A new capture id is published (`VoiceState::capture_id`, phase
   `Listening`), the phrase ends at the usual end silence, is recognized and
   emitted as a `VoiceTranscript` with the new source
   `TranscriptSource::BargeIn`. In wake word mode the phrase is taken as the
   command (no wake word needed).
4. The UI already treats a new transcript as a new spoken request and cancels
   a reply that is still being written.

While JARVIS speaks in wake word mode, phrases are no longer sent to the
wake word recognizer (which mostly heard JARVIS itself); speaking over
JARVIS, with or without «Джарвіс», goes through the gate.

### Errors and limits

- A microphone that cannot open does not stop the speech: the reply is read
  to the end and the error is shown as for other voice errors.
- No speech after barge-in within the existing no-speech timeout ends the
  listener without a request.
- The gate never sends audio anywhere; audio stays in memory as before.

## Interfaces that change

- `VoiceSettings.interruptByVoice` (IPC, settings file; old files default to
  `true`).
- `TranscriptSource` gains `bargeIn` (IPC).
- `VoiceBackend::open_output` takes the playback meter.
- `VoiceEngine::speak_reply()` (barge-in) next to `speak()` (none);
  `jarvis-core` uses `speak_reply()` for voice turns.

## Testing

- `spoken`: dictionary words, acronyms, compounds, rule-based words, numbers
  (thousands, negatives, decimals, times, percentages and their plurals),
  symbols, apostrophes, and that Cyrillic text passes through unchanged.
- `echo`: synthetic signals — echo alone (scaled, delayed copies of the
  reference plus noise) never triggers, also at high volume; speech over the
  echo, speech in a pause and speech with headphones (no echo) trigger
  within 300 ms; nothing triggers during the learning period.
- Engine (fake devices): a voice reply opens the microphone only when the
  setting is on; speech over the reply stops it and emits a `bargeIn`
  transcript whose audio starts at the user's speech; `say()` never opens
  the microphone; wake word mode takes the phrase as a command.
- Live, with the user's consent, on the user's laptop: JARVIS reads a long
  reply through the speakers without interrupting itself; the user
  interrupts it and the new request is answered. With and without
  headphones.

## Documentation

ADR 0011 (barge-in and the text for Ukrainian voices), SECURITY.md (the
microphone during spoken replies), CHANGELOG, ROADMAP (echo cancellation
stays a planned item).
