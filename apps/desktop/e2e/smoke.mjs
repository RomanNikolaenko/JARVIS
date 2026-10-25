#!/usr/bin/env node
// Release smoke test: starts a built JARVIS binary with a fresh profile and
// checks it end to end. No dependencies beyond Node.js 22+.
//
//   node e2e/smoke.mjs <path to jarvis(.exe)> [--plugin <server.mjs>] [--keep-profile]
//
// Windows: drives the WebView2 window over the Chrome DevTools Protocol —
// every screen renders without errors, a chat turn with the diagnostic echo
// model, optionally an MCP server added on the Plugins screen, start-up time
// and memory, and a clean shutdown.
// Linux: WebKitGTK has no DevTools endpoint, so the test checks that the
// core starts, stays up and shuts down cleanly (run it under xvfb-run).
//
// The profile (JARVIS_PROFILE_DIR) is a new temporary folder, so the user's
// real settings and data are never touched.
//
// The DevTools port comes from WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, which
// WebView2 150+ ignores when the app runs elevated (as on GitHub's Windows
// runners). There the workflow sets the port as a machine policy and passes
// it in JARVIS_SMOKE_DEVTOOLS_PORT (docs/RELEASE.md).

import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';

const args = process.argv.slice(2);
const binary = args.find((a) => !a.startsWith('--') && args[args.indexOf(a) - 1] !== '--plugin');
const pluginServer = args.includes('--plugin') ? args[args.indexOf('--plugin') + 1] : null;
const keepProfile = args.includes('--keep-profile');
if (!binary || !fs.existsSync(binary)) {
  console.error('usage: node e2e/smoke.mjs <path to jarvis binary> [--plugin <server.mjs>] [--keep-profile]');
  process.exit(2);
}

const START_TIMEOUT_MS = 90_000;
const STEP_TIMEOUT_MS = 30_000;
const EXIT_TIMEOUT_MS = 30_000;
const IDLE_WINDOW_MS = 5_000;
/** Share of time the renderer's main thread may be busy on the idle dashboard. */
const MAX_IDLE_BUSY = 0.15;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const results = [];
const metrics = {};

/** GitHub Actions annotation (public, unlike job logs). */
function annotate(title, text) {
  if (process.env.GITHUB_ACTIONS === 'true') {
    const encode = (value) => value.replaceAll('%', '%25').replaceAll('\r', '').replaceAll('\n', '%0A');
    console.log(`::error title=${encode(title).replaceAll(':', '%3A').replaceAll(',', '%2C')}::${encode(text)}`);
  }
}

function check(name, ok, detail = '') {
  results.push({ name, ok, detail });
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
  if (!ok) {
    annotate(`Smoke test (${process.platform}): ${name}`, detail || 'failed');
  }
}

async function until(what, probe, timeout = STEP_TIMEOUT_MS) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await probe().catch(() => undefined);
    if (value) {
      return value;
    }
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await sleep(250);
  }
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

function logText(profile) {
  const dir = path.join(profile, 'logs');
  if (!fs.existsSync(dir)) {
    return '';
  }
  return fs
    .readdirSync(dir)
    .filter((name) => name.startsWith('jarvis'))
    .map((name) => fs.readFileSync(path.join(dir, name), 'utf8'))
    .join('\n');
}

function workingSetMb(pid) {
  try {
    if (process.platform === 'win32') {
      const line = execFileSync('tasklist', ['/FI', `PID eq ${pid}`, '/FO', 'CSV', '/NH'], { encoding: 'utf8' });
      const kb = Number(
        line
          .split('","')
          .at(-1)
          .replace(/[^0-9]/g, '')
      );
      return Math.round(kb / 1024);
    }
    const kb = Number(fs.readFileSync(`/proc/${pid}/status`, 'utf8').match(/VmRSS:\s+(\d+)/)?.[1]);
    return Math.round(kb / 1024);
  } catch {
    return null;
  }
}

// Reports the WebView2 browser process, its listening ports, the runtime
// version and WebView2 policies (PowerShell, as -EncodedCommand).
const WEBVIEW_PROBE = String.raw`
$ProgressPreference = 'SilentlyContinue'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$elevated = ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
"user $($identity.Name), elevated: $($elevated), session $((Get-Process -Id $PID).SessionId)"
$webviews = @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'")
"webview processes: $($webviews.Count)"
$webviews | Where-Object { $_.CommandLine -match '--webview-exe-name=jarvis.exe' -and $_.CommandLine -notmatch '--type=' } | ForEach-Object {
  $switch = [regex]::Match($_.CommandLine, '--remote-debugging-port=\d+').Value
  "JARVIS browser process $($_.ProcessId), debugging switch: $(if ($switch) { $switch } else { 'none' })"
  "  $($_.CommandLine)"
}
Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
  Where-Object { $_.OwningProcess -in $webviews.ProcessId } |
  ForEach-Object { "listening $($_.LocalAddress):$($_.LocalPort) (pid $($_.OwningProcess))" }
foreach ($key in 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
                 'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
                 'HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}') {
  $version = (Get-ItemProperty $key -ErrorAction SilentlyContinue).pv
  if ($version) { "WebView2 runtime $version ($($key))" }
}
foreach ($key in 'HKLM:\SOFTWARE\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments',
                 'HKCU:\SOFTWARE\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments',
                 'HKLM:\SOFTWARE\Policies\Microsoft\Edge') {
  if (Test-Path $key) { "policy $($key): " + ((Get-ItemProperty $key | Out-String) -replace '\s+', ' ') }
}
$data = $env:WEBVIEW2_USER_DATA_FOLDER
"webview data folder $($data) exists: $(Test-Path $data)"
$active = Join-Path $data 'EBWebView\DevToolsActivePort'
if (Test-Path $active) { "DevToolsActivePort: $((Get-Content $active) -join ' ')" }
`;

/** What WebView2 is doing when its DevTools endpoint does not answer. */
async function webviewDiagnostics(port, pid, env) {
  const lines = [];
  for (const route of ['/json/version', '/json']) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}${route}`, { signal: AbortSignal.timeout(5_000) });
      lines.push(`GET :${port}${route}: ${response.status} ${(await response.text()).slice(0, 800)}`);
    } catch (error) {
      lines.push(`GET :${port}${route}: ${error.cause?.code ?? error.message}`);
    }
  }
  const run = (program, programArgs) => {
    try {
      const options = { env, encoding: 'utf8', timeout: 30_000, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] };
      return execFileSync(program, programArgs, options)
        .trim()
        .split(/\r?\n/)
        .map((line) => line.slice(0, 700))
        .join('\n');
    } catch (error) {
      return `${program} failed: ${error.message.split('\n')[0]}`;
    }
  };
  lines.push(`app window: ${run('tasklist', ['/V', '/FI', `PID eq ${pid}`, '/FO', 'CSV', '/NH'])}`);
  const script = Buffer.from(WEBVIEW_PROBE, 'utf16le').toString('base64');
  lines.push(run('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', script]));
  return lines.join('\n');
}

/** A minimal CDP client over Node's WebSocket. */
async function connect(port) {
  const target = await until(
    'the WebView DevTools endpoint',
    async () => {
      const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
      return targets.find((t) => t.type === 'page' && !t.url.startsWith('about:'));
    },
    START_TIMEOUT_MS
  );
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
  });
  let next = 1;
  const pending = new Map();
  const errors = [];
  ws.onmessage = (message) => {
    const data = JSON.parse(message.data);
    if (data.id && pending.has(data.id)) {
      pending.get(data.id)(data);
      pending.delete(data.id);
    } else if (data.method === 'Runtime.exceptionThrown') {
      errors.push(data.params.exceptionDetails.exception?.description ?? data.params.exceptionDetails.text);
    } else if (data.method === 'Runtime.consoleAPICalled' && data.params.type === 'error') {
      errors.push(data.params.args.map((a) => a.value ?? a.description).join(' '));
    }
  };
  const send = (method, params = {}) =>
    new Promise((resolve) => {
      const id = next++;
      pending.set(id, resolve);
      ws.send(JSON.stringify({ id, method, params }));
    });
  const evaluate = async (expression) => {
    const reply = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (reply.result?.exceptionDetails) {
      throw new Error(reply.result.exceptionDetails.exception?.description ?? 'evaluation failed');
    }
    return reply.result?.result?.value;
  };
  await send('Runtime.enable');
  return { ws, send, evaluate, errors };
}

// Helpers evaluated in the page.
const PAGE_HELPERS = `
  window.__smoke = {
    button: (text, root = document) =>
      [...root.querySelectorAll('button')].find((b) => b.textContent.trim().startsWith(text)),
    input: (element, value) => {
      element.value = value;
      element.dispatchEvent(new Event('input', { bubbles: true }));
      element.dispatchEvent(new Event('change', { bubbles: true }));
    },
  };
  true
`;

async function windowsChecks(page, profile, pid, started) {
  await until('the app shell', () => page.evaluate(`document.querySelectorAll('.nav-link').length > 5`));
  metrics.uiReadyMs = Date.now() - started;
  // Only now: the DevTools target can appear before the app's document
  // replaces the initial one, which would drop helpers added earlier.
  await page.evaluate(PAGE_HELPERS);
  const status = await until('a core status', () =>
    page.evaluate(`(() => {
      const text = document.querySelector('jv-status-badge')?.textContent.trim() ?? '';
      return /Online|Limited|Faulty|offline/i.test(text) ? text : '';
    })()`)
  );
  let healthDetail = status;
  if (!/Online|Limited/i.test(status)) {
    // Which components are not healthy (the UI only shows the summary).
    const report = await page
      .evaluate(
        `window.__TAURI_INTERNALS__.invoke('get_health').then((h) => JSON.stringify(h.components.filter((c) => c.status !== 'healthy')))`
      )
      .catch((error) => `health report unavailable: ${error.message}`);
    healthDetail = `${status}: ${report}`;
  }
  check('core reachable from the UI', /Online|Limited/i.test(status), healthDetail);

  // Every screen renders, without an error banner.
  const links = await page.evaluate(`[...document.querySelectorAll('.nav-link')].map((a) => a.getAttribute('href'))`);
  for (const href of links) {
    await page.evaluate(`document.querySelector('a.nav-link[href="${href}"]').click(); true`);
    const title = await until(`the ${href} screen`, () =>
      page.evaluate(`(() => {
        const main = document.querySelector('main');
        const heading = main?.querySelector('h1')?.textContent.trim();
        return location.pathname === '${href}' && heading ? heading : '';
      })()`)
    );
    const banner = await page.evaluate(`document.querySelector('.banner[role="alert"]')?.textContent.trim() ?? ''`);
    check(`screen ${href} renders`, !banner, banner || title);
  }

  // A streamed chat turn with the diagnostic echo model.
  await page.evaluate(`document.querySelector('a.nav-link[href="/chat"]').click(); true`);
  await until('the chat composer', () => page.evaluate(`!!document.querySelector('textarea[aria-label="Message"]')`));
  await page.evaluate(`(() => {
    const select = [...document.querySelectorAll('main select')].find((s) =>
      [...s.options].some((o) => o.text.includes('Echo (diagnostic)')));
    const option = [...select.options].find((o) => o.text.includes('Echo (diagnostic)'));
    __smoke.input(select, option.value);
    return true;
  })()`);
  await page.evaluate(`(() => {
    __smoke.input(document.querySelector('textarea[aria-label="Message"]'), 'smoke test message');
    return true;
  })()`);
  await page.evaluate(
    `setTimeout(() => __smoke.button('Send')?.click() ?? document.querySelector('.composer button.primary').click(), 50); true`
  );
  const reply = await until('the echo reply', () =>
    page.evaluate(
      `[...document.querySelectorAll('.message.assistant .bubble')].map((b) => b.textContent).find((t) => t.includes('smoke test message')) ?? ''`
    )
  );
  check('chat turn with the echo model', reply.includes('[echo]'), reply.trim().slice(0, 60));

  // An MCP server on the Plugins screen.
  if (pluginServer) {
    await page.evaluate(`document.querySelector('a.nav-link[href="/plugins"]').click(); true`);
    await until('the Plugins screen', () => page.evaluate(`!!__smoke.button('Add an MCP server')`));
    await page.evaluate(`__smoke.button('Add an MCP server').click(); true`);
    await until('the server form', () => page.evaluate(`!!document.querySelector('form.server-form')`));
    await page.evaluate(`(() => {
      const form = document.querySelector('form.server-form');
      const field = (name) => form.querySelector('[formcontrolname="' + name + '"]');
      __smoke.input(field('name'), 'Smoke tools');
      __smoke.input(field('program'), 'node');
      __smoke.input(field('args'), ${JSON.stringify(path.resolve(pluginServer))});
      setTimeout(() => __smoke.button('Review', form).click(), 50);
      return true;
    })()`);
    await until('the plugin review', () => page.evaluate(`!!__smoke.button('Add and start')`), 60_000);
    await page.evaluate(`__smoke.button('Add and start').click(); true`);
    const plugin = await until(
      'the plugin to run',
      () =>
        page.evaluate(`(() => {
          const panel = document.querySelector('jv-hud-panel.plugin');
          const status = panel?.querySelector('.status')?.textContent.trim() ?? '';
          return /Running|Stopped|Needs/.test(status) ? { status, tools: panel.querySelectorAll('table.tools tbody tr').length } : null;
        })()`),
      60_000
    );
    check(
      'MCP server added and running',
      plugin.status === 'Running' && plugin.tools > 0,
      `${plugin.status}, ${plugin.tools} tools`
    );
  }

  // The idle dashboard must not keep the renderer busy (animations have to
  // run on the compositor; layout on every frame once cost half a core).
  await page.evaluate(`document.querySelector('a.nav-link[href="/dashboard"]').click(); true`);
  await sleep(3_000);
  await page.send('Performance.enable');
  const counters = async () =>
    Object.fromEntries((await page.send('Performance.getMetrics')).result.metrics.map((m) => [m.name, m.value]));
  const before = await counters();
  await sleep(IDLE_WINDOW_MS);
  const after = await counters();
  const busy = (after.TaskDuration - before.TaskDuration) / (IDLE_WINDOW_MS / 1000);
  metrics.idleRendererBusyPercent = Math.round(busy * 1000) / 10;
  metrics.idleLayoutsPerSecond = Math.round((after.LayoutCount - before.LayoutCount) / (IDLE_WINDOW_MS / 1000));
  check(
    'idle dashboard keeps the renderer mostly free',
    busy < MAX_IDLE_BUSY,
    `${metrics.idleRendererBusyPercent} % busy, ${metrics.idleLayoutsPerSecond} layouts/s`
  );

  const heap = await page.send('Runtime.getHeapUsage');
  metrics.jsHeapMb = Math.round((heap.result?.usedSize ?? 0) / 1024 / 1024);
  metrics.processMb = workingSetMb(pid);
  check('no errors in the WebView console', page.errors.length === 0, page.errors.slice(0, 3).join(' | '));
}

async function main() {
  // The long form of the path: CI runners hand out 8.3 temp paths.
  const profile = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'jarvis-smoke-')));
  const port = Number(process.env.JARVIS_SMOKE_DEVTOOLS_PORT) || (await freePort());
  const env = { ...process.env, JARVIS_PROFILE_DIR: profile };
  if (process.platform === 'win32') {
    env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = `--remote-debugging-port=${port}`;
    // A WebView2 browser process of its own: one that a running JARVIS
    // already owns for the default folder would ignore the arguments above.
    env.WEBVIEW2_USER_DATA_FOLDER = path.join(profile, 'webview');
  }
  const started = Date.now();
  const child = spawn(path.resolve(binary), [], { env, stdio: 'ignore' });
  let exited = null;
  child.on('exit', (code, signal) => (exited = { code, signal }));
  console.log(`started ${binary} (pid ${child.pid}), profile ${profile}`);

  try {
    await until('the core to start', async () => logText(profile).includes('JARVIS core ready') || exited, START_TIMEOUT_MS);
    check('core started', !exited, exited ? `exited early: ${JSON.stringify(exited)}` : '');
    metrics.coreReadyMs = Date.now() - started;
    metrics.coreInitMs = Number(logText(profile).match(/JARVIS core ready in (\d+) ms/)?.[1]) || null;

    if (process.platform === 'win32') {
      const page = await connect(port);
      try {
        await windowsChecks(page, profile, child.pid, started);
      } finally {
        page.ws.close();
      }
    } else {
      await sleep(10_000);
      check('still running after 10 s', !exited, exited ? JSON.stringify(exited) : '');
      metrics.processMb = workingSetMb(child.pid);
    }
  } catch (error) {
    check('smoke run', false, error.message);
    if (process.platform === 'win32' && !exited && error.message.includes('DevTools')) {
      const report = await webviewDiagnostics(port, child.pid, env);
      console.log(report);
      annotate('Smoke test (win32): WebView diagnostics', report);
    }
  }

  // A clean shutdown: close the window (WM_CLOSE on Windows, SIGTERM on Linux).
  if (!exited) {
    if (process.platform === 'win32') {
      try {
        execFileSync('taskkill', ['/PID', String(child.pid)], { stdio: 'ignore' });
      } catch {
        // Reported below if the process does not exit.
      }
    } else {
      child.kill('SIGTERM');
    }
    await until('the app to exit', async () => exited, EXIT_TIMEOUT_MS).catch(() => undefined);
  }
  check('app exited after closing', !!exited, exited ? JSON.stringify(exited) : 'still running, killed');
  if (!exited) {
    child.kill('SIGKILL');
  }
  if (process.platform === 'win32') {
    check('core shut down cleanly', logText(profile).includes('JARVIS core shutting down'));
  }

  console.log(`\nmetrics: ${JSON.stringify(metrics)}`);
  if (results.some((r) => !r.ok)) {
    // The end of JARVIS's own log explains most failures.
    const tail = logText(profile).split('\n').filter(Boolean).slice(-40).join('\n');
    annotate(`Smoke test (${process.platform}): JARVIS log`, tail || 'no log written');
  }
  if (!keepProfile) {
    try {
      fs.rmSync(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 500 });
    } catch (error) {
      // WebView2 helper processes can outlive the app for a moment.
      console.warn(`profile ${profile} not removed: ${error.message}`);
    }
  }
  const failed = results.filter((r) => !r.ok);
  console.log(`\nsmoke test: ${results.length - failed.length} passed, ${failed.length} failed`);
  process.exit(failed.length ? 1 : 0);
}

await main();
