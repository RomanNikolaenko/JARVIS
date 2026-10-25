#!/usr/bin/env node
// Writes the update feed (latest.json) for a release from the signed
// installers in a folder (ADR 0014).
//
//   node .github/scripts/latest-json.mjs <folder> <tag> <owner/repo>
//
// Every entry needs the installer and its .sig file. The updater rejects the
// whole feed if one entry is malformed, so the x64 NSIS installer and
// AppImage are required, and nothing partial is written.

import fs from 'node:fs';
import path from 'node:path';

const [dir, tag, repo] = process.argv.slice(2);
if (!dir || !/^v\d+\.\d+\.\d+/.test(tag ?? '') || !/^[\w.-]+\/[\w.-]+$/.test(repo ?? '')) {
  console.error('usage: node latest-json.mjs <folder> <tag like v0.1.0> <owner/repo>');
  process.exit(2);
}

const files = fs.readdirSync(dir);
const base = `https://github.com/${repo}/releases/download/${tag}/`;

// How the bundler names each architecture in file names.
const ARCH = {
  x86_64: /(_x64[_-]|_amd64\.|\.x86_64\.)/,
  aarch64: /(_arm64[_-]|_arm64\.|_aarch64\.|\.aarch64\.)/,
};
// Installer kinds by file name.
const KIND = {
  nsis: (name) => name.endsWith('-setup.exe'),
  msi: (name) => name.endsWith('.msi'),
  appimage: (name) => name.endsWith('.AppImage'),
  deb: (name) => name.endsWith('.deb'),
  rpm: (name) => name.endsWith('.rpm'),
};

/** The signed installer of one kind and architecture, or null. */
function entry(kind, arch) {
  const file = files.find((name) => KIND[kind](name) && ARCH[arch].test(name));
  if (!file || !files.includes(`${file}.sig`)) {
    return null;
  }
  return {
    // The .sig file holds the base64 signature; whitespace is not part of it.
    signature: fs.readFileSync(path.join(dir, `${file}.sig`), 'utf8').trim(),
    url: base + encodeURIComponent(file),
  };
}

// The updater looks for `<os>-<arch>-<bundle type>` first, then `<os>-<arch>`.
const platforms = {};
for (const arch of Object.keys(ARCH)) {
  const nsis = entry('nsis', arch);
  const appimage = entry('appimage', arch);
  if (nsis) {
    platforms[`windows-${arch}`] = nsis;
    platforms[`windows-${arch}-nsis`] = nsis;
  }
  const msi = entry('msi', arch);
  if (msi) platforms[`windows-${arch}-msi`] = msi;
  if (appimage) {
    platforms[`linux-${arch}`] = appimage;
    platforms[`linux-${arch}-appimage`] = appimage;
  }
  for (const kind of ['deb', 'rpm']) {
    const found = entry(kind, arch);
    if (found) platforms[`linux-${arch}-${kind}`] = found;
  }
}
if (!platforms['windows-x86_64'] || !platforms['linux-x86_64']) {
  console.error('the signed x64 NSIS installer and AppImage are required');
  process.exit(1);
}

const feed = {
  version: tag.replace(/^v/, ''),
  notes: `Release notes: https://github.com/${repo}/releases/tag/${tag}`,
  pub_date: new Date().toISOString(),
  platforms,
};
fs.writeFileSync(path.join(dir, 'latest.json'), `${JSON.stringify(feed, null, 2)}\n`);
console.log(`latest.json: ${feed.version}, ${Object.keys(platforms).join(', ')}`);
