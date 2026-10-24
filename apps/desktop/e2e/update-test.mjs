#!/usr/bin/env node
// End-to-end test of signed updates (Windows, ADR 0014). Needs the owner's
// signing key, so it is a manual release check, not part of CI:
//
//   $env:TAURI_SIGNING_PRIVATE_KEY_PATH = "$HOME\.tauri\jarvis-updater.key"
//   $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "<password>"
//   cargo build -p jarvis-desktop --features tauri/custom-protocol   # after `npx ng build`
//   node e2e/update-test.mjs ../../target/debug/jarvis-desktop.exe
//
// A debug build is used because release builds accept only HTTPS feeds; the
// signature check is the same. The "installer" is a tiny program compiled
// here that records its arguments. Scenarios:
// 1. The feed announces 9.9.10 for a file signed as 9.9.9 -> refused.
// 2. The file was changed after signing -> refused, JARVIS keeps running.
// 3. The genuine file -> JARVIS stops its work, starts the installer, exits.

import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';

const binary = process.argv[2];
if (process.platform !== 'win32' || !binary || !fs.existsSync(binary)) {
  console.error('usage (Windows): node e2e/update-test.mjs <debug jarvis-desktop.exe>');
  process.exit(2);
}
if (!process.env.TAURI_SIGNING_PRIVATE_KEY_PATH && !process.env.TAURI_SIGNING_PRIVATE_KEY) {
  console.error('set TAURI_SIGNING_PRIVATE_KEY_PATH (and _PASSWORD) to the update signing key');
  process.exit(2);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, detail = '') => {
  results.push(ok);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};
async function until(what, probe, timeout = 60_000) {
  const end = Date.now() + timeout;
  for (;;) {
    const value = await probe().catch(() => undefined);
    if (value) return value;
    if (Date.now() > end) throw new Error(`timed out: ${what}`);
    await sleep(300);
  }
}

// ---- the signed payload
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'jarvis-update-test-'));
const marker = path.join(work, 'installer-ran.txt');
fs.writeFileSync(
  path.join(work, 'installer.rs'),
  `fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _ = std::fs::write(${JSON.stringify(marker)}, args.join(" "));
}\n`
);
execFileSync('rustc', ['-O', 'installer.rs', '-o', 'setup.exe'], { cwd: work });
fs.copyFileSync(path.join(work, 'setup.exe'), path.join(work, 'tampered.exe'));
fs.appendFileSync(path.join(work, 'tampered.exe'), 'x');
execFileSync(
  process.execPath,
  [
    path.resolve('node_modules/@tauri-apps/cli/tauri.js'),
    'signer',
    'sign',
    '--app-version',
    '9.9.9',
    path.join(work, 'setup.exe'),
  ],
  { stdio: 'ignore' }
);
const signature = fs.readFileSync(path.join(work, 'setup.exe.sig'), 'utf8').trim();

// ---- the feed
let feedVersion = '9.9.9';
let feedFile = 'setup.exe';
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  if (url.pathname === '/latest.json') {
    res.setHeader('content-type', 'application/json');
    res.end(
      JSON.stringify({
        version: feedVersion,
        notes: 'Test release',
        pub_date: '2026-10-24T12:00:00Z',
        platforms: {
          'windows-x86_64': { signature, url: `http://127.0.0.1:${server.address().port}/download/${feedFile}` },
        },
      })
    );
  } else if (url.pathname.startsWith('/download/')) {
    res.end(fs.readFileSync(path.join(work, path.basename(url.pathname))));
  } else {
    res.statusCode = 404;
    res.end();
  }
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));

// ---- JARVIS
const profile = path.join(work, 'profile');
const devtoolsPort = 9472;
const app = spawn(path.resolve(binary), [], {
  env: {
    ...process.env,
    JARVIS_PROFILE_DIR: profile,
    JARVIS_UPDATE_ENDPOINT: `http://127.0.0.1:${server.address().port}/latest.json`,
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${devtoolsPort}`,
  },
  stdio: 'ignore',
});
let exited = null;
app.on('exit', (code) => (exited = { code }));

const target = await until('DevTools', async () =>
  (await (await fetch(`http://127.0.0.1:${devtoolsPort}/json`)).json()).find(
    (t) => t.type === 'page' && !t.url.startsWith('about:')
  )
);
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let next = 1;
const pending = new Map();
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  pending.get(d.id)?.(d);
  pending.delete(d.id);
};
const evaluate = (expression) =>
  new Promise((resolve) => {
    const id = next++;
    pending.set(id, (d) => resolve(d.result?.result?.value));
    ws.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, returnByValue: true, awaitPromise: true } }));
  });
const panelText = () => evaluate(`document.querySelector('#updates')?.innerText ?? ''`);
const button = (text) =>
  `[...document.querySelectorAll('#updates button')].find((b) => b.textContent.trim().startsWith(${JSON.stringify(text)}))`;
const click = (text) => evaluate(`(() => { const b = ${button(text)}; b?.click(); return !!b; })()`);
const idle = () => evaluate(`(() => { const b = ${button('Check now')}; return !!b && !b.disabled; })()`);

let lastRefusal = '';
async function attempt(name, version, file, refused) {
  await until(`${name}: idle`, idle);
  feedVersion = version;
  feedFile = file;
  await click('Check now');
  await until(`${name}: update found`, async () => (await panelText()).includes(`version ${version} is available`));
  await click(`Install ${version}`);
  await click('Install and restart');
  if (refused) {
    const line = await until(`${name}: refusal`, async () => {
      const found = (await panelText()).split('\n').find((l) => /signature|failed/i.test(l)) ?? '';
      return found && found !== lastRefusal && (await idle()) ? found : '';
    });
    lastRefusal = line;
    check(`${name}: refused, JARVIS keeps running`, !exited, line);
  }
}

try {
  await until('the app shell', () => evaluate(`document.querySelectorAll('.nav-link').length > 5`));
  await evaluate(`document.querySelector('a.nav-link[href="/settings"]').click(); true`);
  await until('the updates panel', () => evaluate(`!!document.querySelector('#updates')`));
  await attempt('the feed announces another version', '9.9.10', 'setup.exe', true);
  await attempt('a changed file', '9.9.9', 'tampered.exe', true);
  await attempt('the genuine update', '9.9.9', 'setup.exe', false);
  await until('JARVIS to exit for the installer', async () => exited);
  check('JARVIS exited to let the installer run', true, JSON.stringify(exited));
  const args = await until('the installer to run', async () => fs.existsSync(marker) && fs.readFileSync(marker, 'utf8'), 20_000);
  check('the signed installer ran with update arguments', /\/UPDATE/.test(args), args);
  const log = fs
    .readdirSync(path.join(profile, 'logs'))
    .map((f) => fs.readFileSync(path.join(profile, 'logs', f), 'utf8'))
    .join('\n');
  check('the core shut down before installing', log.includes('JARVIS core shutting down'));
} catch (error) {
  check('update test', false, error.message);
} finally {
  ws.close();
  if (!exited) app.kill();
  server.close();
  await sleep(2000);
  try {
    fs.rmSync(work, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
  } catch {
    console.log(`left ${work}`);
  }
}
const failed = results.filter((ok) => !ok).length;
console.log(`\nupdate test: ${results.length - failed} passed, ${failed} failed`);
process.exit(failed ? 1 : 0);
