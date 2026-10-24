# Releasing JARVIS

## What a release contains

| Platform | Files | Notes |
| --- | --- | --- |
| Windows 10/11 x64 | `JARVIS_<version>_x64-setup.exe` (NSIS), `JARVIS_<version>_x64_en-US.msi` | NSIS installs for the current user, no administrator rights; installs WebView2 if missing |
| Linux x64 | `.deb` (Debian/Ubuntu), `.rpm` (Fedora/openSUSE), `.AppImage` | built on Ubuntu 22.04; needs WebKitGTK 4.1 and ALSA |
| All | `SHA256SUMS.txt` | checksums of every file |

**Not signed yet.** Windows shows a SmartScreen warning for unsigned
installers, and there are no automatic updates: both need keys that only the
project owner can create and keep (an Authenticode certificate, an update
signing key). Users verify downloads with `SHA256SUMS.txt`.

## Steps

1. Update the version in `Cargo.toml` (`[workspace.package]`) and
   `apps/desktop/package.json`; add the release to `CHANGELOG.md`.
2. Make sure CI is green on `main` (the desktop job runs the release smoke
   test on Windows and Linux).
3. Tag and push: `git tag v<version>` and `git push origin v<version>`.
4. The **Release** workflow builds the installers on Windows and Ubuntu
   22.04, runs the smoke test against the release binary, and creates a
   **draft** GitHub release with the files and checksums.
5. Check the draft (manual checklist below), then publish it.

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
- [ ] Uninstall: the app is removed; user data stays (settings, data and logs
      folders listed in DEVELOPMENT.md).
