# 0016 — Mentor mode: a voice teacher for building an app

- Status: Accepted
- Date: 2026-11-13

## Context

The owner wants to learn technologies by building a real application with
JARVIS as a teacher: turn the mode on once, talk without the wake word,
have JARVIS say what to write, how and why, check what was actually written,
speak up after a save, and, on request, type code into the file slowly
while explaining it. The owner codes in VS Code and Zed. Design:
[docs/superpowers/specs/2026-11-13-mentor-mode-design.md](../superpowers/specs/2026-11-13-mentor-mode-design.md).

## Decision

Mentor mode is a conversation with a teacher prompt; JARVIS gathers the
context itself (approach B of the design). An agent loop (the model reading
files through tools) was rejected for latency: through Claude Code every
step is another CLI run. Editor extensions were rejected as two extensions
plus a channel for a first version.

- **Listening:** a `Conversation` listen mode in `jarvis-voice` emits every
  phrase as a request (`TranscriptSource::Conversation`) while the
  microphone stays open; talking over a reply stops it (same barge-in
  detector). Starting the mode is explicit and logged; the indicator shows
  it on every screen.
- **Session:** `mentor_start` needs a project folder inside a workspace
  folder and outside JARVIS's own folders, and, for a cloud model,
  `privacy.allowCloudAgents` ("Let agents use cloud AI providers"). The link
  conversation → folder is stored (migration 6, `mentor_sessions`).
- **Context per turn:** a snapshot of the folder (text files, size and
  modification time; tool and build folders skipped; at most 5 000 files;
  files over 256 KiB by name only). A mentor turn sends the mentor prompt,
  the file list on the first turn and the full text of changed files (up to
  30 % of the context window, newest first) with the request only; the saved
  message stays the user's phrase. Project content is quoted as untrusted
  data.
- **Plan:** the mentor keeps the lesson plan in a `<plan>` block that becomes
  the conversation's pinned note and is never spoken.
- **Writing:** a `<write>` block (path, `create` or `replace` with an exact
  `<find>` fragment, `<say>`/`<code>` parts) becomes a `write_file` or
  `edit_file` call: the tool's checks, the Security Policy Engine (the tool
  must be on in Settings → Tools) and an approval that is always asked
  (overwriting is high risk). After approval JARVIS speaks each explanation
  and writes at most 3 lines every 150 ms. Before every write it compares the
  file with its own last write and stops, keeping the user's text, when they
  differ. Every step is audited; refusals and stops are saved as short notes
  in the conversation for the mentor.
- **Speaking up on save:** a watcher polls sizes and modification times every
  second while a lesson runs and asks the UI for a turn when the user's files
  have been quiet for 2 s: never while the UI is busy, the lesson is paused
  or JARVIS types; at most once per 20 s; at most 3 times in a row before the
  user speaks again. JARVIS's own writes do not count.
- **Spoken commands** (`пауза`, `закінчуємо урок`, `подивись на екран`, `так`,
  `ні`, `стоп`, `продовжуємо` and English equivalents) are matched as whole
  phrases in the core and never reach the model. "Yes" approves a pending
  new file or edit by voice; overwriting needs a click.

## Consequences

- The project's files and, on request, screenshots reach the chosen model;
  a cloud model gets them only with the existing permissions.
- While JARVIS types, the file holds partial code; a running dev server may
  rebuild on each step.
- There is no echo cancellation of JARVIS's own: without a headset the
  microphone array of the laptop must cancel the speakers, or JARVIS's voice
  may be heard as the user (the barge-in echo gate reduces this).
- The write-block format must be produced by the model; a malformed block is
  refused with a note, nothing is written.
