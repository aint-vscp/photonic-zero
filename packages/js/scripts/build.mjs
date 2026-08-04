#!/usr/bin/env node
/**
 * Builds the WebAssembly module and copies it next to the JavaScript.
 *
 * The .wasm is a build artifact and is deliberately not committed; this runs
 * from `npm run build` and again from `prepublishOnly`, so a published tarball
 * always contains a module built from the sources in the same commit.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const packageRoot = resolve(here, '..');
const repoRoot = resolve(packageRoot, '..', '..');
const manifest = join(repoRoot, 'crates', 'pz-wasm', 'Cargo.toml');

if (!existsSync(manifest)) {
  console.error(`cannot find ${manifest}`);
  console.error('This script builds from the Rust sources and must run inside a checkout.');
  process.exit(1);
}

console.log('building pz-wasm for wasm32-unknown-unknown...');
try {
  execFileSync(
    'cargo',
    [
      'build',
      '--release',
      '--target',
      'wasm32-unknown-unknown',
      '--manifest-path',
      manifest,
    ],
    { stdio: 'inherit' },
  );
} catch {
  console.error('\ncargo build failed.');
  console.error('Install Rust from https://rustup.rs and add the wasm target:');
  console.error('  rustup target add wasm32-unknown-unknown');
  process.exit(1);
}

const built = join(
  repoRoot, 'crates', 'pz-wasm', 'target',
  'wasm32-unknown-unknown', 'release', 'pz_wasm.wasm',
);
if (!existsSync(built)) {
  console.error(`cargo reported success but ${built} is missing`);
  process.exit(1);
}

mkdirSync(join(packageRoot, 'src'), { recursive: true });
const destination = join(packageRoot, 'src', 'pz.wasm');
copyFileSync(built, destination);

// The licences are duplicated into the package so the published tarball is
// self-contained; npm shows them on the package page.
for (const licence of ['LICENSE-MIT', 'LICENSE-APACHE']) {
  const from = join(repoRoot, licence);
  if (existsSync(from)) copyFileSync(from, join(packageRoot, licence));
}

const { size } = statSync(destination);
console.log(`wrote src/pz.wasm (${size.toLocaleString()} bytes)`);
