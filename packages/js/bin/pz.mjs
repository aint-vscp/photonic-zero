#!/usr/bin/env node
/**
 * The `pz` command line tool, runnable with `npx photonic-zero`.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

import { mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { load, ProgressKind } from '../src/index.mjs';

const USAGE = `\
pz - Photonic Zero: data over light, from a screen to a camera

USAGE
    npx photonic-zero <command> [options]

COMMANDS
    encode <input>    Encode a file into a sequence of optical frames
    decode <files>    Decode captured frames back into the original file
    selftest          Run an end-to-end transfer in memory

ENCODE
    -o, --out DIR        Output directory (default: pz-frames)
    -n, --frames N       Frames to write. The stream is endless, so more frames
                         only means more resilience. Default is 1.5x the
                         minimum plus a few.
    -p, --profile NAME   balanced | robust | fast | resilient
        --module-px N    Pixels per cell (default 8)
        --quiet N        Quiet zone in cells (default 4)
        --session N      Pin the session id instead of deriving it

DECODE
    -o, --out FILE       Write the payload here (default: standard output)

    Reads PNGs produced by pz. A directory may be given instead of files.

EXAMPLES
    npx photonic-zero encode secret.txt -o frames --profile robust
    npx photonic-zero decode frames -o recovered.txt
    npx photonic-zero selftest
`;

function parseArgs(argv) {
  const positional = [];
  const flags = new Map();
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg.startsWith('--')) {
      const [name, inline] = arg.slice(2).split('=');
      flags.set(name, inline ?? argv[++i]);
    } else if (arg.length === 2 && arg.startsWith('-') && arg !== '-') {
      flags.set(arg.slice(1), argv[++i]);
    } else {
      positional.push(arg);
    }
  }
  return { positional, flags };
}

function flag(flags, short, long, fallback) {
  const value = flags.get(short) ?? flags.get(long);
  return value === undefined ? fallback : value;
}

function collectFiles(inputs) {
  const files = [];
  for (const input of inputs) {
    if (statSync(input).isDirectory()) {
      for (const name of readdirSync(input).sort()) {
        if (name.toLowerCase().endsWith('.png')) files.push(join(input, name));
      }
    } else {
      files.push(input);
    }
  }
  return files;
}

async function cmdEncode(positional, flags) {
  const source = positional[0];
  if (!source) throw new Error('encode needs an input file');

  const payload = source === '-' ? readFileSync(0) : readFileSync(source);
  if (payload.length === 0) throw new Error('input is empty');

  const pz = await load();
  const profile = flag(flags, 'p', 'profile', 'balanced');
  const sessionRaw = flag(flags, '', 'session', undefined);
  const encoder = pz.encode(payload, {
    profile,
    sessionId: sessionRaw === undefined ? undefined : Number(sessionRaw),
  });

  const outDir = flag(flags, 'o', 'out', 'pz-frames');
  mkdirSync(outDir, { recursive: true });

  const modulePx = Number(flag(flags, '', 'module-px', 8));
  const quietZone = Number(flag(flags, '', 'quiet', 4));
  const minimum = encoder.blockCount;
  const count = Number(
    flag(flags, 'n', 'frames', minimum + Math.floor(minimum / 2) + 4),
  );

  for (let index = 0; index < count; index++) {
    const png = encoder.framePNG(index, { modulePx, quietZone });
    writeFileSync(
      join(outDir, `frame${String(index).padStart(5, '0')}.png`),
      png,
    );
  }

  console.error(`encoded ${payload.length} bytes`);
  console.error(`  grid        ${encoder.modules}x${encoder.modules} cells (${profile})`);
  console.error(`  per frame   ${encoder.dropletSize} bytes`);
  console.error(`  minimum     ${minimum} frames`);
  console.error(`  written     ${count} frames to ${outDir}`);
  console.error(`  session     0x${encoder.sessionId.toString(16).toUpperCase().padStart(4, '0')}`);
  encoder.free();
}

async function cmdDecode(positional, flags) {
  if (positional.length === 0) throw new Error('decode needs at least one frame file');
  const files = collectFiles(positional);
  if (files.length === 0) throw new Error('no PNG frames found');

  const pz = await load();
  const decoder = pz.decoder();
  let recovered = null;

  for (const file of files) {
    let status;
    try {
      status = decoder.ingestPNG(readFileSync(file));
    } catch (error) {
      console.error(`  skipping ${file}: ${error.message}`);
      continue;
    }
    if (status.kind === ProgressKind.Complete) {
      recovered = decoder.result();
      break;
    }
    if (status.kind === ProgressKind.Progressed) {
      console.error(`  ${file}: ${status.recovered}/${status.total} blocks`);
    }
  }

  if (recovered === null) {
    throw new Error(
      `not enough frames: recovered ${(decoder.progress * 100).toFixed(0)}% ` +
        `after ${decoder.framesSeen} images`,
    );
  }

  console.error(
    `recovered ${recovered.length} bytes from ${decoder.framesAccepted} of ${decoder.framesSeen} images`,
  );

  const out = flag(flags, 'o', 'out', undefined);
  if (out) writeFileSync(out, recovered);
  else process.stdout.write(recovered);
  decoder.free();
}

async function cmdSelftest() {
  const pz = await load();
  console.log(`Photonic Zero self test (wire format version ${pz.protocolVersion})\n`);

  const payload = new Uint8Array(4096);
  for (let i = 0; i < payload.length; i++) payload[i] = (i * 37 + 11) & 0xff;

  for (const profile of ['robust', 'balanced', 'resilient', 'fast']) {
    const encoder = pz.encode(payload, { profile });
    const decoder = pz.decoder();
    const minimum = encoder.blockCount;

    let index = 0;
    let done = null;
    while (done === null) {
      // Drop one frame in four, as a hand-held camera would.
      if (index % 4 !== 3) {
        const { width, height, data } = encoder.frameRGBA(index, { modulePx: 4 });
        const status = decoder.ingestRGBA(width, height, data);
        if (status.kind === ProgressKind.Complete) done = decoder.result();
      }
      if (++index > 20000) throw new Error(`${profile}: never converged`);
    }

    const same =
      done.length === payload.length && done.every((b, i) => b === payload[i]);
    if (!same) throw new Error(`${profile}: recovered bytes do not match`);

    console.log(
      `  ${profile.padEnd(10)} ${String(encoder.modules).padStart(3)} cells  ` +
        `${String(encoder.dropletSize).padStart(4)} B/frame  ` +
        `minimum ${String(minimum).padStart(4)}  ` +
        `used ${String(decoder.framesAccepted).padStart(4)}  OK`,
    );
    encoder.free();
    decoder.free();
  }

  console.log('\nall profiles round-tripped 4096 bytes with 25% frame loss');
}

const [command, ...rest] = process.argv.slice(2);
const { positional, flags } = parseArgs(rest);

try {
  switch (command) {
    case 'encode': await cmdEncode(positional, flags); break;
    case 'decode': await cmdDecode(positional, flags); break;
    case 'selftest': await cmdSelftest(); break;
    case 'help': case '--help': case '-h': case undefined:
      process.stdout.write(USAGE);
      break;
    case 'version': case '--version': case '-V': {
      const pkg = JSON.parse(
        readFileSync(new URL('../package.json', import.meta.url), 'utf8'),
      );
      console.log(`pz ${pkg.version}`);
      break;
    }
    default:
      process.stderr.write(`pz: unknown command '${command}'\n\n${USAGE}`);
      process.exit(1);
  }
} catch (error) {
  console.error(`pz: ${error.message}`);
  process.exit(1);
}
