# Releasing JARVIS

## What a release contains

| Platform | Files | Notes |
| --- | --- | --- |
| Windows 10/11 x64 | `JARVIS_<version>_x64-setup.exe` (NSIS), `JARVIS_<version>_x64_en-US.msi` | NSIS installs for the current user, no administrator rights; installs WebView2 if missing |
| Linux x64 | `.deb` (Debian/Ubuntu), `.rpm` (Fedora/openSUSE), `.AppImage` | built on Ubuntu 22.04; needs WebKitGTK 4.1 and ALSA |
| Windows 11 ARM64 | `JARVIS_<version>_arm64-setup.exe`, `JARVIS_<version>_arm64_en-US.msi` | built on `windows-11-arm` |
| Linux ARM64 | `_arm64.deb`, `.aarch64.rpm`, `_aarch64.AppImage` | built on `ubuntu-22.04-arm` |

On ARM64, voice (whisper.cpp, Piper) is not available: only x64 engine
packages are pinned. The embedded runtime installs the official llama.cpp
ARM64 CPU builds.
| All | `*.sig`, `latest.json` | update signatures and the update feed read by installed copies |
| All | `SHA256SUMS.txt` | checksums of every file |

**Updates are signed** (ADR 0014): the installers are the update files, each
with a minisign signature (`.sig`) bound to the release version. Installed
copies find new versions in `latest.json` of the latest *published* release
and install them only after the user confirms. **The installers are not
Authenticode-signed**, so Windows shows a SmartScreen warning on the first
install; users can verify downloads with `SHA256SUMS.txt`.

## The update signing key (once)

The key pair was created with `tauri signer generate`:

- private key: `~/.tauri/jarvis-updater.key` (encrypted with a password);
- password: `~/.tauri/jarvis-updater.key.password` — move it into a
  password manager and delete the file;
- public key: `~/.tauri/jarvis-updater.key.pub`, built into the app
  (`plugins.updater.pubkey` in `tauri.conf.json`).

Back up the private key and its password. Without them installed copies can
no longer be updated automatically; a new key means a manual reinstall for
everyone. Never commit them.

Add two repository secrets (GitHub → Settings → Secrets and variables →
Actions), used only by the Release workflow:

| Secret | Value |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | the content of `jarvis-updater.key` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the password |

The workflow stops with an error when they are missing. Local builds do not
need them: updater artifacts are only made with
`--config src-tauri/tauri.release.conf.json`.

## Steps

1. Update the version in `Cargo.toml` (`[workspace.package]`) and
   `apps/desktop/package.json`; add the release to `CHANGELOG.md`.
2. Make sure CI is green on `main` (the desktop job runs the release smoke
   test on Windows and Linux).
3. Tag and push: `git tag v<version>` and `git push origin v<version>`.
4. The **Release** workflow builds and signs the installers on Windows and
   Ubuntu 22.04, runs the smoke test against the release binary, writes
   `latest.json` (`.github/scripts/latest-json.mjs`) and creates a **draft**
   GitHub release with the files, signatures and checksums.
5. Check the draft (manual checklist below), then publish it. Publishing
   makes the update visible to installed copies.

The workflow refuses a tag that does not match the workspace version.

## Release smoke test

`apps/desktop/e2e/smoke.mjs` starts a built binary with a new temporary
profile (`JARVIS_PROFILE_DIR`), so it never touches real data:

```bash
node apps/desktop/e2e/smoke.mjs target/release/jarvis.exe --plugin examples/plugins/text-tools/server.mjs
```

On Windows it drives the window through the WebView2 DevTools protocol:
the core status, every screen, a streamed chat turn with the diagnostic echo
model, an MCP server added on the Plugins screen, no console errors, start-up
time and memory, and a clean shutdown. On Linux (run under `xvfb-run`) it
checks start-up, that the app stays up, and exit.

## Update test (manual, Windows, needs the key)

`apps/desktop/e2e/update-test.mjs` signs a stand-in installer with the real
key, serves a local feed and drives a debug build: a feed that announces a
different version than the signed one is refused, a changed file is refused
(JARVIS keeps running), and the genuine file makes JARVIS stop its work, start
the installer with the update arguments and exit.

```powershell
cd apps/desktop
npx ng build
cargo build -p jarvis-desktop --features tauri/custom-protocol
$env:TAURI_SIGNING_PRIVATE_KEY_PATH = "$HOME\.tauri\jarvis-updater.key"
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "<password>"
node e2e/update-test.mjs ../../target/debug/jarvis-desktop.exe
```

## Manual checklist (per release)

- [ ] Install from each installer on a clean machine (or VM); the app starts
      and the dashboard shows the core online or limited.
- [ ] Ollama or LM Studio: models listed, a chat reply streams and can be
      stopped.
- [ ] Embedded runtime: install llama.cpp (CPU build), download a curated
      model, chat, stop.
- [ ] Memory: a saved conversation and a fact survive a restart.
- [ ] Agents: a read-only task; a change that asks for approval (deny, then
      allow); the audit log verifies.
- [ ] Voice: microphone indicator, push-to-talk, a spoken reply.
- [ ] Vision: a screenshot question to a vision model.
- [ ] Plugins: the example plugin from a folder; a changed plugin asks for
      review again.
- [ ] Updates: an installed previous version finds the new release after it
      is published (Settings → Updates → Check now) and installs it.
- [ ] Uninstall: the app is removed; user data stays (settings, data and logs
      folders listed in DEVELOPMENT.md).
