// Keeps the Rust build folder (target/ at the repository root) in bounds.
// `pnpm run tauri …` runs it first (package.json), like AstraBrowser does.
//
// Cargo never deletes old builds: every compiler or dependency version and
// every mode (dev, tests, Clippy, release, features) leaves its files in
// target, and the folder had grown to 175 GB, 126 GB of it the incremental
// compilation cache. So:
// - incremental caches (<profile>/incremental) not touched for STALE_DAYS
//   days are removed: they belong to old versions and modes; the newest cache
//   of every crate stays, however old, or that crate would rebuild in full;
// - if target is still larger than LIMIT_GB, it is removed entirely, like
//   `cargo clean`: the next build starts from scratch (several minutes).
// Does nothing on CI, which builds on clean machines. Another folder can be
// passed as an argument: node scripts/clean-target.mjs <folder>.
import { existsSync, readdirSync, rmSync, statSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';

const STALE_DAYS = 7;
const LIMIT_GB = 40;
const DAY = 24 * 60 * 60 * 1000;
const GB = 1024 ** 3;

const target = resolve(process.argv[2] ?? join(import.meta.dirname, '../../../target'));
if (process.env.CI || !existsSync(target)) {
  process.exit(0);
}

/** Folders inside `dir` (none when it does not exist). */
function dirs(dir) {
  try {
    return readdirSync(dir, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => join(dir, entry.name));
  } catch {
    return [];
  }
}

/** When a folder last changed: itself or what is directly in it (rustc sessions). */
function modified(dir) {
  return readdirSync(dir).reduce((latest, name) => Math.max(latest, statSync(join(dir, name)).mtimeMs), statSync(dir).mtimeMs);
}

function size(dir) {
  let total = 0;
  for (const entry of readdirSync(dir, { withFileTypes: true, recursive: true })) {
    if (entry.isFile()) {
      total += statSync(join(entry.parentPath, entry.name)).size;
    }
  }
  return total;
}

/** Removes a folder; a file in use (a running jarvis-desktop.exe) is no obstacle: Cargo rebuilds what is missing. */
function remove(dir) {
  try {
    rmSync(dir, { recursive: true, force: true, maxRetries: 2 });
  } catch (error) {
    console.warn(`target: not everything was removed (${error.code ?? error.message})`);
  }
}

const gb = (bytes) => (bytes / GB).toFixed(1);

// <profile>/incremental and <target triple>/<profile>/incremental.
const caches = [...dirs(target), ...dirs(target).flatMap(dirs)]
  .map((dir) => join(dir, 'incremental'))
  .filter((dir) => existsSync(dir));
const stale = [];
for (const cache of caches) {
  const crates = Map.groupBy(
    dirs(cache).map((dir) => ({ dir, time: modified(dir) })),
    ({ dir }) => basename(dir).replace(/-[^-]+$/, '')
  );
  for (const versions of crates.values()) {
    const newest = Math.max(...versions.map(({ time }) => time));
    stale.push(...versions.filter(({ time }) => time < newest && Date.now() - time > STALE_DAYS * DAY).map(({ dir }) => dir));
  }
}
if (stale.length) {
  const freed = stale.reduce((total, dir) => total + size(dir), 0);
  stale.forEach(remove);
  console.log(`target: removed stale compilation caches (${stale.length} folders, ${gb(freed)} GB)`);
}

const total = size(target);
if (total > LIMIT_GB * GB) {
  remove(target);
  console.log(`target: ${gb(total)} GB is over ${LIMIT_GB} GB, removed entirely; Rust builds from scratch (several minutes)`);
}
