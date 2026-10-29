#!/usr/bin/env node
// Install test: installs a built package the way a user would, checks the
// installed files, runs the release smoke test against the installed program,
// uninstalls it and checks that the program is gone while user data stays.
// No dependencies beyond Node.js 22+.
//
//   node e2e/install-test.mjs <nsis|msi|deb|appimage> <bundle folder> [--plugin <server.mjs>] [--allow-install]
//
// It changes the machine (installs JARVIS, writes a marker into the user data
// folder), so outside CI it runs only with --allow-install, and never when
// JARVIS is already installed. The MSI installs for all users and needs an
// elevated shell; deb uses sudo unless run as root; the AppImage is extracted,
// not installed. On Windows the test also removes the installers' registry
// key afterwards when it did not exist before.

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const [kind, bundleDir] = process.argv.slice(2);
const args = process.argv.slice(4);
const pluginServer = args.includes('--plugin') ? path.resolve(args[args.indexOf('--plugin') + 1]) : null;
const KINDS = {
  nsis: { folder: 'nsis', match: (name) => name.endsWith('-setup.exe') },
  msi: { folder: 'msi', match: (name) => name.endsWith('.msi') },
  deb: { folder: 'deb', match: (name) => name.endsWith('.deb') },
  appimage: { folder: 'appimage', match: (name) => name.endsWith('.AppImage') },
};
if (!KINDS[kind] || !bundleDir) {
  console.error(
    'usage: node e2e/install-test.mjs <nsis|msi|deb|appimage> <bundle folder> [--plugin <server.mjs>] [--allow-install]'
  );
  process.exit(2);
}
if (process.env.GITHUB_ACTIONS !== 'true' && !args.includes('--allow-install')) {
  console.error('This test installs and removes JARVIS on this machine; pass --allow-install to run it outside CI.');
  process.exit(2);
}

const folder = path.resolve(bundleDir, KINDS[kind].folder);
const packages = fs.existsSync(folder) ? fs.readdirSync(folder).filter(KINDS[kind].match) : [];
if (packages.length !== 1) {
  console.error(`expected one ${kind} package in ${folder}, found ${packages.length}`);
  process.exit(2);
}
const installer = path.join(folder, packages[0]);
const LICENSE_FILES = ['LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt'];
const results = [];

/** GitHub Actions annotation (public, unlike job logs). */
function annotate(title, text) {
  if (process.env.GITHUB_ACTIONS === 'true') {
    const encode = (value) => value.replaceAll('%', '%25').replaceAll('\r', '').replaceAll('\n', '%0A');
    console.log(`::error title=${encode(title).replaceAll(':', '%3A').replaceAll(',', '%2C')}::${encode(text)}`);
  }
}

function check(name, ok, detail = '') {
  results.push({ name, ok });
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
  if (!ok) {
    annotate(`Install test (${kind}): ${name}`, detail || 'failed');
  }
  return ok;
}

function run(program, programArgs, options = {}) {
  const result = spawnSync(program, programArgs, { encoding: 'utf8', windowsHide: true, ...options });
  const output = `${result.stdout ?? ''}${result.stderr ?? ''}`.trim();
  return { code: result.status, output: result.error ? result.error.message : output };
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function until(probe, timeout) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (probe()) {
      return true;
    }
    await sleep(500);
  }
  return probe();
}

const asRoot = (program, programArgs) =>
  process.getuid?.() === 0 ? run(program, programArgs) : run('sudo', ['-n', program, ...programArgs]);

function elevated() {
  return run('net', ['session']).code === 0;
}

/** Where each kind installs, and how to install and remove it. */
const UNINSTALL_KEY = 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\JARVIS';
// The MSI installs into the folder of an earlier installation (the NSIS
// uninstaller leaves its path under this key) and records its own as
// InstallDir, which the MSI uninstall removes.
const VENDOR_KEY = 'HKCU\\Software\\JARVIS project';
const PRODUCT_KEY = `${VENDOR_KEY}\\JARVIS`;

/** A string value from the registry, or null. */
function registryValue(key, name) {
  const output = run('reg', ['query', key, '/v', name]).output;
  return output.match(new RegExp(`${name}\\s+REG_\\w+\\s+"?([^"\\r\\n]+?)"?\\s*$`, 'm'))?.[1] ?? null;
}

const platform = {
  nsis: {
    installed: () => run('reg', ['query', UNINSTALL_KEY]).code === 0,
    install: () => run(installer, ['/S']),
    location: () => registryValue(UNINSTALL_KEY, 'InstallLocation'),
    binary: (dir) => path.join(dir, 'jarvis.exe'),
    shortcut: path.join(process.env.APPDATA ?? '', 'Microsoft/Windows/Start Menu/Programs/JARVIS.lnk'),
    uninstall: (dir) => run(path.join(dir, 'uninstall.exe'), ['/S']),
    userData: path.join(process.env.APPDATA ?? '', 'ai.jarvis.desktop'),
  },
  msi: {
    installed: () => registryValue(PRODUCT_KEY, 'InstallDir') !== null,
    install: () =>
      run('msiexec', ['/i', installer, '/qn', '/norestart', '/l*v', path.join(os.tmpdir(), 'jarvis-msi-install.log')]),
    location: () => registryValue(PRODUCT_KEY, 'InstallDir'),
    binary: (dir) => path.join(dir, 'jarvis.exe'),
    shortcut: path.join(process.env.ProgramData ?? '', 'Microsoft/Windows/Start Menu/Programs/JARVIS/JARVIS.lnk'),
    uninstall: () => run('msiexec', ['/x', installer, '/qn', '/norestart']),
    userData: path.join(process.env.APPDATA ?? '', 'ai.jarvis.desktop'),
  },
  deb: {
    name: () => run('dpkg-deb', ['-f', installer, 'Package']).output,
    installed: () => run('dpkg', ['-s', platform.deb.name()]).output.includes('Status: install ok installed'),
    install: () => asRoot('apt-get', ['install', '-y', '--no-install-recommends', installer]),
    location: () => '/',
    binary: () => '/usr/bin/jarvis',
    shortcut: '/usr/share/applications/JARVIS.desktop',
    uninstall: () => asRoot('apt-get', ['remove', '-y', platform.deb.name()]),
    userData: path.join(os.homedir(), '.config', 'ai.jarvis.desktop'),
  },
};

/** Files below a folder, with relative paths. */
function listFiles(dir) {
  return fs
    .readdirSync(dir, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile() || entry.isSymbolicLink());
}

function smoke(binary, env = process.env) {
  const smokeArgs = [path.join(import.meta.dirname, 'smoke.mjs'), binary, ...(pluginServer ? ['--plugin', pluginServer] : [])];
  const result = spawnSync(process.execPath, smokeArgs, { stdio: 'inherit', env });
  return check('smoke test against the installed program', result.status === 0, `exit ${result.status}`);
}

async function testAppImage() {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), 'jarvis-appimage-'));
  try {
    fs.chmodSync(installer, 0o755);
    const extracted = run(installer, ['--appimage-extract'], { cwd: work });
    const root = path.join(work, 'squashfs-root');
    if (
      !check(
        'AppImage extracts',
        extracted.code === 0 && fs.existsSync(root),
        extracted.code === 0 ? '' : extracted.output.slice(-500)
      )
    ) {
      return;
    }
    const names = new Set(listFiles(root).map((entry) => entry.name));
    for (const name of LICENSE_FILES) {
      check(`AppImage contains ${name}`, names.has(name));
    }
    smoke(path.join(root, 'AppRun'));
  } finally {
    fs.rmSync(work, { recursive: true, force: true });
  }
}

async function testInstaller() {
  const target = platform[kind];
  if (kind === 'msi' && !elevated()) {
    console.log('SKIPPED: the MSI installs for all users and needs an elevated shell (CI runs it).');
    process.exit(0);
  }
  if (target.installed()) {
    console.error('JARVIS is already installed here; the test does not touch an existing installation.');
    process.exit(2);
  }
  const vendorKeyExisted = process.platform !== 'win32' || run('reg', ['query', VENDOR_KEY]).code === 0;

  const installed = target.install();
  if (
    !check(`install ${path.basename(installer)}`, installed.code === 0, installed.code === 0 ? '' : installed.output.slice(-800))
  ) {
    return;
  }
  const dir = target.location();
  const binary = dir ? target.binary(dir) : '';
  const programInstalled = check(
    'program installed',
    !!binary && fs.existsSync(binary),
    binary || 'install location not registered'
  );
  if (kind === 'deb') {
    const files = run('dpkg', ['-L', target.name()]).output.split('\n');
    for (const name of LICENSE_FILES) {
      const file = files.find((line) => line.endsWith(`/${name}`));
      check(`installed ${name}`, !!file && fs.existsSync(file), file ?? 'not in the package');
    }
  } else {
    for (const name of LICENSE_FILES) {
      check(`installed ${name}`, fs.existsSync(path.join(dir, name)));
    }
  }
  check('menu shortcut created', fs.existsSync(target.shortcut), target.shortcut);

  if (programInstalled) {
    smoke(binary);
  }

  // User data must survive the uninstall. The marker goes into the settings
  // folder; a folder created here is removed again at the end.
  const createdUserData = !fs.existsSync(target.userData);
  const marker = path.join(target.userData, `install-test-${process.pid}.marker`);
  fs.mkdirSync(target.userData, { recursive: true });
  fs.writeFileSync(marker, 'kept by the uninstaller\n');

  const removed = target.uninstall(dir);
  check('uninstall', removed.code === 0, removed.code === 0 ? '' : removed.output.slice(-800));
  // The NSIS uninstaller copies itself to a temporary folder and returns at once.
  const gone = await until(() => !fs.existsSync(binary) && !target.installed(), 60_000);
  if (programInstalled) {
    check('program removed', gone, gone ? '' : `${binary} or its registration is still there`);
    check('menu shortcut removed', !fs.existsSync(target.shortcut), target.shortcut);
    if (kind !== 'deb') {
      check('install folder removed', !fs.existsSync(dir), dir);
    }
  }
  check('user data kept', fs.existsSync(marker), target.userData);

  fs.rmSync(createdUserData ? target.userData : marker, { recursive: true, force: true });
  // Leave the machine as found: the NSIS uninstaller keeps the install path
  // for the next installer, and the MSI leaves an empty key.
  if (!vendorKeyExisted) {
    run('reg', ['delete', VENDOR_KEY, '/f']);
  }
}

console.log(`install test: ${kind}, ${installer}`);
if (kind === 'appimage') {
  await testAppImage();
} else {
  await testInstaller();
}
const failed = results.filter((r) => !r.ok);
console.log(`\ninstall test (${kind}): ${results.length - failed.length} passed, ${failed.length} failed`);
process.exit(failed.length ? 1 : 0);
