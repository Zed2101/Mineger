// Builds the playit.gg agent daemon (playitd) from the official source and
// drops it where Tauri expects the sidecar:
//   src-tauri/binaries/playitd-x86_64-pc-windows-msvc.exe
//
//   npm run playit:build
//
// playit-agent is BSD-2-Clause (https://github.com/playit-cloud/playit-agent);
// the tag below is the release Mineger was tested with. Needs git and the
// Rust toolchain. Nothing is modified or rebranded: it is the upstream daemon.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const TAG = 'v1.0.10';
const REPO = 'https://github.com/playit-cloud/playit-agent.git';
const TRIPLE = 'x86_64-pc-windows-msvc';

const root = path.resolve(import.meta.dirname, '..');
const dest = path.join(root, 'src-tauri', 'binaries', `playitd-${TRIPLE}.exe`);
const work = path.join(os.tmpdir(), `mineger-playit-${TAG}`);

const run = (cmd, args, cwd) => {
  const r = spawnSync(cmd, args, { stdio: 'inherit', cwd, shell: process.platform === 'win32' });
  if (r.status !== 0) {
    console.error(`\n${cmd} ${args.join(' ')} failed`);
    process.exit(r.status ?? 1);
  }
};

if (process.argv.includes('--check')) {
  if (fs.existsSync(dest)) {
    console.log(`playitd present: ${dest} (${Math.round(fs.statSync(dest).size / 1024)} KB)`);
    process.exit(0);
  }
  console.error(`playitd missing: ${dest}\nRun: npm run playit:build`);
  process.exit(1);
}

if (!fs.existsSync(path.join(work, 'Cargo.toml'))) {
  fs.rmSync(work, { recursive: true, force: true });
  console.log(`Cloning playit-agent ${TAG}…`);
  run('git', ['clone', '--depth', '1', '--branch', TAG, REPO, work]);
}
console.log('Building playitd (release)…');
run('cargo', ['build', '-p', 'playitd', '--bin', 'playitd', '--release'], work);

const built = path.join(work, 'target', 'release', 'playitd.exe');
if (!fs.existsSync(built)) {
  console.error(`Build finished but ${built} is missing`);
  process.exit(1);
}
fs.mkdirSync(path.dirname(dest), { recursive: true });
fs.copyFileSync(built, dest);
console.log(`playitd ${TAG} → ${dest}`);
