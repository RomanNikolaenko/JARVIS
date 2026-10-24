#!/usr/bin/env node
// Writes the update feed (latest.json) for a release from the signed
// installers in a folder (ADR 0014).
//
//   node .github/scripts/latest-json.mjs <folder> <tag> <owner/repo>
//
// Every entry needs the installer and its .sig file. The updater rejects the
// whole feed if one entry is malformed, so a missing required installer
// fails the script instead of producing a partial feed.

import fs from 'node:fs';
import path from 'node:path';

const [dir, tag, repo] = process.argv.slice(2);
if (!dir || !/^v\d+\.\d+\.\d+/.test(tag ?? '') || !/^[\w.-]+\/[\w.-]+$/.test(repo ?? '')) {
  console.error('usage: node latest-json.mjs <folder> <tag like v0.1.0> <owner/repo>');
  process.exit(2);
}

const files = fs.readdirSync(dir);
const base = `https://github.com/${repo}/releases/download/${tag}/`;

/** The installer matching `test` with its signature, or null. */
function entry(test) {
  const file = files.find((name) => test(name) && !name.endsWith('.sig'));
  if (!file || !files.includes(`${file}.sig`)) {
    return null;
  }
  return {
    // The .sig file holds the base64 signature; whitespace is not part of it.
    signature: fs.readFileSync(path.join(dir, `${file}.sig`), 'utf8').trim(),
    url: base + encodeURIComponent(file),
  };
}

const nsis = entry((name) => name.endsWith('-setup.exe'));
const msi = entry((name) => name.endsWith('.msi'));
const appimage = entry((name) => name.endsWith('.AppImage'));
const deb = entry((name) => name.endsWith('.deb'));
const rpm = entry((name) => name.endsWith('.rpm'));
if (!nsis || !appimage) {
  console.error('the signed NSIS installer and AppImage are required');
  process.exit(1);
}

// The updater looks for `<os>-<arch>-<bundle type>` first, then `<os>-<arch>`.
const platforms = {
  'windows-x86_64': nsis,
  'windows-x86_64-nsis': nsis,
  'linux-x86_64': appimage,
  'linux-x86_64-appimage': appimage,
};
if (msi) platforms['windows-x86_64-msi'] = msi;
if (deb) platforms['linux-x86_64-deb'] = deb;
if (rpm) platforms['linux-x86_64-rpm'] = rpm;

const feed = {
  version: tag.replace(/^v/, ''),
  notes: `Release notes: https://github.com/${repo}/releases/tag/${tag}`,
  pub_date: new Date().toISOString(),
  platforms,
};
fs.writeFileSync(path.join(dir, 'latest.json'), `${JSON.stringify(feed, null, 2)}\n`);
console.log(`latest.json: ${feed.version}, ${Object.keys(platforms).join(', ')}`);
