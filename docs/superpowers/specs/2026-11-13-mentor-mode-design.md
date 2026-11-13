# Mentor mode: a voice teacher that builds an app with you

Date: 2026-11-13. Status: approved by the owner (2026-11-13).

## Intent

The owner wants to learn technologies by building a real application with
JARVIS as a teacher. They turn the mode on once, say what they are building
and with which technologies, and from then on talk to JARVIS by voice without
switching it off. JARVIS leads: it says what to write next, how, and why, and
watches what the owner actually wrote. When the owner says "write it
yourself", JARVIS types the code into the file slowly, line by line, while it
explains each part aloud. The owner codes in VS Code and Zed.

Success: a lesson where the owner never touches the JARVIS window between
"let's start" and "let's stop", hears a short spoken answer about 2 s after
finishing a phrase, and JARVIS's advice always refers to the code as it is on
disk right now.

## Goals

- **Conversation listening.** One switch starts continuous listening without
  the wake word: every phrase the owner says is a message to the mentor.
  JARVIS answers aloud; talking over it interrupts it (existing barge-in).
- **Teacher role.** A mentor conversation keeps a lesson plan (goal,
  technologies, steps, current step) and explains what, how and why in
  short spoken answers; code never gets read aloud character by character.
- **Sees the project.** Before every answer JARVIS adds the project files
  that changed since the previous turn, so the mentor knows what was written.
- **Speaks up by itself.** When the owner saves files, JARVIS reacts without
  being asked: confirms the step, points out a mistake, or says what is next.
- **Sees the screen on request.** "Подивись на екран" (or a button) attaches
  a screenshot to that turn.
- **Writes slowly on request.** "Напиши сам" makes the mentor propose a
  concrete change; after the owner confirms it, JARVIS types it into the file
  in step with the spoken explanation, and the editor shows it appear.

## Non-goals (first version)

- VS Code or Zed extensions, cursor tracking, typing into the editor window.
- Running, building or testing the owner's code (no `run_command`).
- An agent loop where the model decides what to read (approach A below).
- Several mentor projects at the same time.
- Cloud speech recognition or synthesis (whisper.cpp and Piper stay).

## Anti-goals (a failure even if it works)

- The microphone open without a visible indicator, or open after the owner
  said "stop" or closed the mode.
- JARVIS writing to a file the owner did not confirm, outside the project
  folder, or over edits the owner made while JARVIS was typing.
- Long lectures: spoken answers over about 4 sentences, or code read aloud.
- Project files or screenshots sent to a cloud model without the existing
  privacy permissions.

## Constraints

- Every write goes through the Security Policy Engine and the approval
  broker; only workspace folders are reachable (existing `jarvis-tools`
  scope rules).
- Microphone rules of ADR 0011: opening and closing are logged, `micOpen`
  drives the indicator on every screen.
- Privacy: a cloud mentor gets project files only with
  `privacy.allowCloudAgents` (the existing "cloud model reads file contents"
  permission) and screenshots only with `privacy.shareCapturesWithCloud`.
  Without them the mode refuses to start with a cloud model and says which
  switch is missing.
- No new dependencies unless a later plan justifies one.
- Latency: one model call per owner phrase. With Claude Code that is about
  1.3–3 s to the first spoken words (measured 2026-11-12).

## Approaches considered

- **A. Mentor as an agent.** Every phrase starts an agent run that reads
  files and takes screenshots through tools as the model decides. Most
  flexible, but every tool step is another model call; through Claude Code
  that is 5–10 s before JARVIS speaks.
- **B. Mentor as a conversation; JARVIS gathers the context (chosen by the
  owner).** A mentor conversation with a teacher prompt and a lesson plan.
  JARVIS itself attaches changed files (and a screenshot on request) to each
  turn; writing is a separate, confirmed action. One model call per phrase,
  and the mentor always sees the current code.
- **C. Editor extensions.** Typing directly in VS Code and Zed. Most natural,
  but two extensions plus a channel to JARVIS; too much for a first version.

## Design

### 1. The mentor session

- Started from the Chat screen ("Start a lesson") or by voice ("Джарвіс,
  починаємо урок"). The owner picks the project folder (must be, or becomes,
  a workspace folder) and the model (the current chat model by default).
- A mentor session is a saved conversation marked as a mentor conversation,
  linked to its project folder. Reopening it later continues the lesson.
- The lesson plan lives in the conversation's pinned note (the existing
  per-conversation working memory, up to 10 % of the context). The mentor
  updates it through a `<plan>…</plan>` block in its reply; JARVIS stores the
  block as the pinned note and never speaks it.
- The first turn asks what to build and with which technologies (unless the
  owner already said it), then writes the plan.

### 2. Conversation listening

- A new listening mode next to push-to-talk, wake word and barge-in: every
  phrase the VAD closes is transcribed and delivered as a transcript of the
  new source `conversation`. The microphone stays open until the mode ends.
- While JARVIS speaks, the existing echo gate and barge-in apply: the owner
  talking over it stops the reply and their phrase becomes the next turn.
- While JARVIS is still thinking (no speech yet), a new phrase is held and
  sent after the reply ends, joined with any other held phrases.
- Local voice commands, recognized before anything goes to the model:
  "пауза" (stop listening but keep the session), "продовжуємо" (only after a
  pause, by push-to-talk or the button), "закінчуємо урок" (end the mode).
  The words are a fixed list in Ukrainian and English.
- Without an installed speech engine and recognition model the mode does not
  start and points to Settings → Voice; without a voice it still runs and
  shows replies as text only. (Builder check: my call.)
- The mode ends on the button, the end phrase, closing the mentor
  conversation, a voice engine error, or JARVIS closing. The indicator says
  "Mentor is listening".

### 3. What the mentor sees

- **Files.** At the start, JARVIS takes a snapshot of the project folder:
  relative path, size and modification time of text files, skipping `.git`,
  `node_modules`, `target`, `dist`, build outputs and anything over 256 KiB.
  Before each turn it compares with the previous snapshot and adds to the
  turn the list of created, changed and deleted files and the full current
  text of changed and created files, up to a budget (about 30 % of the
  context window, the most recently changed first; the rest listed by name).
  The first turn adds a file tree (names only).
- **Speaking up on save (owner's choice).** While the mode listens, JARVIS
  checks the project folder about once a second (modification times only;
  no file-watch dependency). When the owner's files changed and then stayed
  unchanged for 2 s, JARVIS starts a turn by itself with those changes and
  the instruction to react briefly: confirm the step, point out a mistake, or
  say what to write next. Guards against an endless loop of its own actions:
  - changes JARVIS typed itself never start a turn;
  - no turn while the owner is speaking, a reply is running or being
    spoken, a phrase is held, or the mode is paused;
  - at most one such turn per 20 s; changes that arrive meanwhile are
    gathered into the next one;
  - after 3 such turns in a row without the owner saying anything, JARVIS
    stops speaking up until the owner speaks again.
- **Screen.** A local phrase check ("подивись на екран", "що на екрані",
  "look at the screen") or the button captures the screen and attaches it to
  that turn. It needs a vision-capable model.
- The project content travels as untrusted data, quoted like tool results;
  instructions inside the owner's files are not instructions to JARVIS.

### 4. Writing slowly

- When the owner asks JARVIS to write, the mentor replies with a write block:
  the target file, the kind of change (new file, or replace an exact
  fragment, the semantics of `write_file` / `edit_file`), and the code split
  into parts, each with a short spoken explanation.
- JARVIS turns the block into a normal tool request through the Security
  Policy Engine (`write_file` new: medium; `edit_file`: medium; overwrite:
  high) and shows the approval dialog with the file and the complete change.
  The owner confirms by click or by saying "так" / "пиши" while the dialog is
  open; "ні" declines. Overwriting an existing file needs a click.
- After approval JARVIS speaks the first explanation, then types that part
  into the file a few lines at a time (writes to disk about every 150 ms, so
  VS Code and Zed reload it), then the next explanation, and so on. At the end
  the file must equal the approved result exactly.
- Before every write it checks that the file on disk is still what JARVIS
  last wrote (size, modification time and content hash). If the owner changed
  it, JARVIS stops typing, leaves the owner's text in place and says so. It
  asks the owner to save the file before typing starts.
- "Стоп" during typing stops it; what was typed stays and the mentor is told
  where it stopped.

### 5. Speech and display

- Spoken text excludes `<plan>`, write blocks and fenced code; the chat shows
  the full reply, with code rendered as usual.
- The mentor prompt asks for at most about 4 sentences per spoken answer and
  one step at a time, in the assistant language.

## Left to the builder

- Exact names of the new types, IPC commands, events and the mentor prompt.
- The exact write-block format (one that a small local model can also
  produce), its parser and the validation of paths and fragments.
- Where the "mentor conversation" mark is stored (a conversation field and a
  migration, or a separate table) and how the project folder link is kept.
- Typing speed and chunking rules, within "a few lines every ~150 ms, in step
  with speech".
- The UI layout of the mentor bar (indicator, pause, screen, end) in Chat.
- Tests: fake voice backend and fake provider for the listening loop, held
  phrases, the voice commands, the snapshot and budget, the write flow
  including a concurrent edit by the owner.
