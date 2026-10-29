#!/usr/bin/env node
// Writes THIRD-PARTY-NOTICES.txt: the licenses of the Rust crates and Node.js
// packages that ship inside JARVIS. MIT, BSD, ISC and Apache-2.0 ask for
// their notices in every copy, so the installers include the file.
//
//   node .github/scripts/third-party-notices.mjs           write the file
//   node .github/scripts/third-party-notices.mjs --check   fail if it is stale
//
// Needs `cargo` and `pnpm install` in apps/desktop: the texts come from the cargo
// registry and node_modules. Crates of the release targets are listed; build
// scripts, proc macros and development dependencies are not shipped and are
// left out.

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

const root = path.resolve(import.meta.dirname, '../..');
const app = path.join(root, 'apps/desktop');
const output = path.join(root, 'THIRD-PARTY-NOTICES.txt');
const check = process.argv.includes('--check');

const LICENSE_FILE = /^(licen[cs]e|copying|notice|copyright|unlicense)([-._].*)?$/i;
/** The platforms the Release workflow builds for. */
const TARGETS = ['x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc', 'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu'];
const RULE = '='.repeat(80);
const THIN_RULE = '-'.repeat(80);

/** License files in a package folder. */
function licenseFiles(dir, extra) {
  const names = fs
    .readdirSync(dir, { withFileTypes: true })
    .filter((entry) => entry.isFile() && LICENSE_FILE.test(entry.name))
    .map((entry) => entry.name);
  if (extra && !names.includes(extra) && fs.existsSync(path.join(dir, extra))) {
    names.push(extra);
  }
  return names.sort();
}

/**
 * The files that cover the license a package is used under: MIT where an
 * expression without AND offers it, otherwise every license file.
 */
function chosenFiles(license, files) {
  const alternatives = (license ?? '')
    .replaceAll('/', ' OR ')
    .split(/\s+OR\s+/)
    .map((part) => part.replace(/[()]/g, '').trim());
  const mit = files.filter((name) => /mit/i.test(name));
  if (!/\bAND\b/.test(license ?? '') && alternatives.includes('MIT') && mit.length) {
    return [...mit, ...files.filter((name) => /^notice/i.test(name))];
  }
  return files;
}

// SPDX texts for packages published without a license file. MIT and BSD get
// the package authors as the copyright holder.
const MIT_BODY = fs.readFileSync(path.join(root, 'LICENSE-MIT'), 'utf8').slice(
  fs.readFileSync(path.join(root, 'LICENSE-MIT'), 'utf8').indexOf('Permission is hereby granted')
);
const BSD_CONDITIONS = [
  'Redistribution and use in source and binary forms, with or without',
  'modification, are permitted provided that the following conditions are met:',
  '',
  '1. Redistributions of source code must retain the above copyright notice, this',
  '   list of conditions and the following disclaimer.',
  '',
  '2. Redistributions in binary form must reproduce the above copyright notice,',
  '   this list of conditions and the following disclaimer in the documentation',
  '   and/or other materials provided with the distribution.',
];
const BSD_THIRD = [
  '',
  '3. Neither the name of the copyright holder nor the names of its',
  '   contributors may be used to endorse or promote products derived from',
  '   this software without specific prior written permission.',
];
const BSD_DISCLAIMER = [
  '',
  'THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"',
  'AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE',
  'IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE',
  'DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE',
  'FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL',
  'DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR',
  'SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER',
  'CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,',
  'OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE',
  'OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.',
];

function copyrightLine(pkg) {
  const authors = (pkg.authors ?? []).map((a) => a.replace(/\s*<[^>]*>/, '').trim()).filter(Boolean);
  return `Copyright (c) ${authors.length ? authors.join(', ') : `the ${pkg.name} authors`}`;
}

/**
 * The SPDX text for a package without a license file, or null. Texts without
 * a copyright line (Apache-2.0, MPL-2.0) come from a package that ships one.
 */
function templateText(pkg, shared) {
  const alternatives = pkg.license.replaceAll('/', ' OR ').split(/\s+OR\s+/).map((part) => part.trim());
  const license = alternatives.includes('MIT') ? 'MIT' : alternatives.length === 1 ? alternatives[0] : null;
  switch (license) {
    case 'MIT':
      return `${copyrightLine(pkg)}\n\n${normalize(MIT_BODY)}`;
    case 'BSD-2-Clause':
      return [copyrightLine(pkg), '', ...BSD_CONDITIONS, ...BSD_DISCLAIMER].join('\n');
    case 'BSD-3-Clause':
      return [copyrightLine(pkg), '', ...BSD_CONDITIONS, ...BSD_THIRD, ...BSD_DISCLAIMER].join('\n');
    default:
      return shared.get(license) ?? null;
  }
}

function normalize(text) {
  return text
    .replace(/^﻿/, '')
    .replaceAll('\r\n', '\n')
    .split('\n')
    .map((line) => line.trimEnd())
    .join('\n')
    .trim();
}

/** Shipped crates: normal dependencies of the workspace, without proc macros. */
function rustPackages() {
  const metadata = JSON.parse(
    execFileSync('cargo', ['metadata', '--format-version', '1', '--locked', ...TARGETS.flatMap((t) => ['--filter-platform', t])], {
      cwd: root,
      encoding: 'utf8',
      maxBuffer: 256 * 1024 * 1024,
    })
  );
  const packages = new Map(metadata.packages.map((p) => [p.id, p]));
  const nodes = new Map(metadata.resolve.nodes.map((n) => [n.id, n]));
  const members = new Set(metadata.workspace_members);
  const shipped = new Set();
  const stack = [...members];
  while (stack.length) {
    const id = stack.pop();
    if (shipped.has(id)) {
      continue;
    }
    const pkg = packages.get(id);
    if (pkg.targets.some((t) => t.kind.includes('proc-macro'))) {
      continue;
    }
    shipped.add(id);
    for (const dep of nodes.get(id)?.deps ?? []) {
      if (dep.dep_kinds.some((k) => k.kind === null)) {
        stack.push(dep.pkg);
      }
    }
  }
  return [...shipped]
    .filter((id) => !members.has(id))
    .map((id) => {
      const pkg = packages.get(id);
      const dir = path.dirname(pkg.manifest_path);
      const extra = pkg.license_file ? path.relative(dir, path.resolve(dir, pkg.license_file)) : null;
      return {
        name: pkg.name,
        version: pkg.version,
        license: pkg.license ?? (extra ? `see ${extra}` : 'unknown'),
        repository: pkg.repository ?? '',
        authors: pkg.authors,
        dir,
        files: licenseFiles(dir, extra),
      };
    });
}

/** Node.js packages the app depends on at run time (pnpm-lock.yaml). */
function nodePackages() {
  const executable = process.platform === 'win32' ? (process.env.ComSpec ?? 'cmd.exe') : 'pnpm';
  const args =
    process.platform === 'win32'
      ? ['/d', '/s', '/c', 'pnpm licenses list --prod --json']
      : ['licenses', 'list', '--prod', '--json'];
  const report = JSON.parse(
    execFileSync(executable, args, {
      cwd: app,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'inherit'],
    })
  );
  const found = new Map();

  for (const entries of Object.values(report)) {
    for (const entry of entries) {
      for (const packagePath of entry.paths ?? []) {
        const dir = fs.realpathSync(packagePath);
        const manifestPath = path.join(dir, 'package.json');
        if (!fs.existsSync(manifestPath)) {
          throw new Error(`${manifestPath} is missing: run pnpm install in apps/desktop first`);
        }
        const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
        const key = `${manifest.name}@${manifest.version}`;
        if (found.has(key)) {
          continue;
        }
        const author = typeof manifest.author === 'string' ? manifest.author : manifest.author?.name;
        const repository =
          typeof manifest.repository === 'string' ? manifest.repository : (manifest.repository?.url ?? entry.homepage ?? '');
        found.set(key, {
          name: manifest.name,
          version: manifest.version,
          license: manifest.license ?? entry.license ?? 'unknown',
          repository,
          authors: author ? [author] : [],
          dir,
          files: licenseFiles(dir),
        });
      }
    }
  }

  return [...found.values()];
}

function render(packages) {
  const groups = new Map();
  const add = (pkg, text) => {
    const key = createHash('sha256').update(text).digest('hex');
    if (!groups.has(key)) {
      groups.set(key, { text, packages: [] });
    }
    groups.get(key).packages.push(pkg);
  };
  const shared = new Map([['Apache-2.0', normalize(fs.readFileSync(path.join(root, 'LICENSE-APACHE'), 'utf8'))]]);
  const unfiled = [];
  for (const pkg of packages) {
    const files = chosenFiles(pkg.license, pkg.files);
    if (!files.length) {
      unfiled.push(pkg);
      continue;
    }
    const text = files.map((name) => normalize(fs.readFileSync(path.join(pkg.dir, name), 'utf8'))).join(`\n\n${THIN_RULE}\n\n`);
    if (pkg.license === 'MPL-2.0' && files.length === 1) {
      shared.set('MPL-2.0', text);
    }
    add(pkg, text);
  }
  const missing = [];
  for (const pkg of unfiled) {
    const text = templateText(pkg, shared);
    if (text) {
      add({ ...pkg, templated: true }, text);
    } else {
      missing.push(pkg);
    }
  }

  const label = (pkg) => `${pkg.name} ${pkg.version}${pkg.templated ? '*' : ''}`;
  const byName = (a, b) => a.name.localeCompare(b.name, 'en') || a.version.localeCompare(b.version, 'en');
  const sections = [...groups.values()]
    .map((group) => ({ ...group, packages: group.packages.sort(byName) }))
    .sort((a, b) => byName(a.packages[0], b.packages[0]));

  const lines = [
    'Third-party software in JARVIS',
    '',
    'JARVIS is licensed under MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE).',
    'It contains the Rust crates and Node.js packages below. Their license texts',
    'follow, one section per identical text. Where a package offers MIT among',
    'other licenses, JARVIS uses it under MIT.',
    '',
    'Packages marked * are published without a license file; their section',
    'gives the standard SPDX text, with the package authors as copyright holders',
    'for MIT and BSD. MPL-2.0 packages are used unmodified; their source code is',
    'available from crates.io and the repositories named in their manifests.',
    '',
    'Generated by .github/scripts/third-party-notices.mjs from Cargo.lock and',
    'apps/desktop/pnpm-lock.yaml; do not edit by hand.',
    '',
  ];
  for (const section of sections) {
    lines.push(RULE, wrap(section.packages.map(label).join(', ')), THIN_RULE, '', section.text, '');
  }
  if (missing.length) {
    lines.push(RULE, 'Packages without a license file in their published package', THIN_RULE, '');
    for (const pkg of missing.sort(byName)) {
      lines.push(`${label(pkg)}: ${pkg.license}${pkg.repository ? `, ${pkg.repository}` : ''}`);
    }
    lines.push('');
  }
  return { text: `${lines.join('\n')}\n`, count: packages.length, sections: sections.length, missing };
}

function wrap(text, width = 80) {
  const lines = [];
  let line = '';
  for (const word of text.split(' ')) {
    if (line && line.length + word.length + 1 > width) {
      lines.push(line);
      line = word;
    } else {
      line = line ? `${line} ${word}` : word;
    }
  }
  return [...lines, line].join('\n');
}

const result = render([...rustPackages(), ...nodePackages()]);
const summary = `${result.count} packages, ${result.sections} license texts, ${result.missing.length} without a license file`;
if (check) {
  const current = fs.existsSync(output) ? normalize(fs.readFileSync(output, 'utf8')) : '';
  if (current !== normalize(result.text)) {
    console.error(`THIRD-PARTY-NOTICES.txt is out of date (${summary}).`);
    console.error('Run: node .github/scripts/third-party-notices.mjs');
    process.exit(1);
  }
  console.log(`THIRD-PARTY-NOTICES.txt is current: ${summary}`);
} else {
  fs.writeFileSync(output, result.text);
  console.log(`THIRD-PARTY-NOTICES.txt: ${summary}, ${Math.round(result.text.length / 1024)} KB`);
}
