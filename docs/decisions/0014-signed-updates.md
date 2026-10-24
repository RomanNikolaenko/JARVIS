# 0014 — Signed automatic updates from GitHub Releases

- Status: Accepted
- Date: 2026-10-24

## Context

JARVIS ships as installers from the Release workflow (NSIS, MSI, deb, rpm,
AppImage). Without updates, users keep old versions with known bugs and
security fixes missing. An update mechanism is also the most dangerous code
path of the app: whoever can make JARVIS install a file runs code with the
user's rights. All other data stays on the computer (ADR 0013); an update
check is the only regular request JARVIS makes on its own.

## Decision

- **Tauri's updater plugin** (`tauri-plugin-updater` 2.14, used only from
  Rust; no updater permission for JavaScript). Default features off: no
  system proxy (like every other HTTP client in JARVIS) and no support for
  the old zip update bundles.
- **Signatures, not just HTTPS.** Every update file is signed with a
  minisign key (`tauri signer`). The public key is built into the app
  (`plugins.updater.pubkey`); the plugin refuses a file whose signature does
  not match. The private key and its password never enter the repository:
  they live with the owner (`~/.tauri/jarvis-updater.key`, password kept
  separately) and as the GitHub secrets `TAURI_SIGNING_PRIVATE_KEY` and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, used only by the Release workflow.
  Losing the private key means installed copies can no longer be updated
  automatically; it must be backed up.
- **Feed:** a static `latest.json` attached to each GitHub release, read from
  `https://github.com/RomanNikolaenko/JARVIS/releases/latest/download/latest.json`
  (only published releases count; drafts are invisible). The Release
  workflow generates it from the signatures: Windows (NSIS by default, MSI
  for MSI installs) and Linux (AppImage by default, deb and rpm for package
  installs, when they are signed).
- **The user decides.** JARVIS checks for updates 30 seconds after start
  and then once a day, if `updates.checkAutomatically` is on (default on);
  the check sends nothing but the request itself. A found update is shown
  with its version and notes; it is downloaded and installed only after the
  user confirms. Before installing, JARVIS stops agent tasks, plugins and
  local models (Windows: the installer closes the app and starts it again;
  Linux: the package is installed, then JARVIS restarts).
- **Tests.** `JARVIS_UPDATE_ENDPOINT` replaces the feed URL (the signature
  check stays). Release builds accept only HTTPS endpoints; a debug build
  can use a local HTTP server for an end-to-end test.

## Consequences

- Updates are only as safe as the private key; anyone who has it (and its
  password) can ship code to every JARVIS user. It is kept out of CI logs
  and the repository.
- Windows installers are still not Authenticode-signed (SmartScreen
  warnings on the first install); the updater does not need Authenticode.
- deb and rpm updates need administrator rights (the plugin asks through
  `pkexec`); the AppImage replaces itself.
- Users who turn automatic checks off update manually (Settings → Updates →
  Check now, or a new installer).
