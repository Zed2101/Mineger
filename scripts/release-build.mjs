// Builds the installers, signs them for the in-app updater and writes the
// manifest the running apps poll (src-tauri/target/release/bundle/latest.json).
//
//   npm run release:build
//
// Needs the private key generated once with
//   npx tauri signer generate -w ~/.tauri/mineger.key -p <password>
// with the password saved in ~/.tauri/mineger.key.password (or pass
// TAURI_SIGNING_PRIVATE_KEY / TAURI_SIGNING_PRIVATE_KEY_PASSWORD in the environment).
// The public half lives in src-tauri/tauri.conf.json → plugins.updater.pubkey.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const REPO = 'Zed2101/Mineger';

function fail(msg) {
  console.error(`\n${msg}\n`);
  process.exit(1);
}

/** Body of the `## [version]` section of the changelog, without the heading. */
function changelogSection(md, version) {
  const out = [];
  let inside = false;
  for (const line of md.split(/\r?\n/)) {
    if (line.startsWith('## ')) {
      if (inside) break;
      inside = line.startsWith(`## [${version}]`);
      continue;
    }
    if (inside) out.push(line);
  }
  return out.join('\n').trim();
}

const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
const conf = JSON.parse(fs.readFileSync(path.join(root, 'src-tauri/tauri.conf.json'), 'utf8'));
const cargo = fs.readFileSync(path.join(root, 'src-tauri/Cargo.toml'), 'utf8').match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const version = pkg.version;
if (conf.version !== version || cargo !== version) {
  fail(`Version mismatch: package.json ${version}, tauri.conf.json ${conf.version}, Cargo.toml ${cargo}. Bump all three first.`);
}
if (!conf.plugins?.updater?.pubkey) fail('tauri.conf.json has no plugins.updater.pubkey');

const notes = changelogSection(fs.readFileSync(path.join(root, 'CHANGELOG.md'), 'utf8'), version);
if (!notes) fail(`CHANGELOG.md has no "## [${version}]" section: move the Unreleased notes under the new version first.`);

const sidecar = path.join(root, 'src-tauri', 'binaries', 'playitd-x86_64-pc-windows-msvc.exe');
if (!fs.existsSync(sidecar)) fail(`playit agent missing at ${sidecar}. Build it once with: npm run playit:build`);

const keyPath = path.join(os.homedir(), '.tauri', 'mineger.key');
const passwordPath = `${keyPath}.password`;
if (!process.env.TAURI_SIGNING_PRIVATE_KEY && !fs.existsSync(keyPath)) {
  fail(`Signing key not found at ${keyPath}. Generate it with: npx tauri signer generate -w ${keyPath} -p <password>\nand save the password in ${passwordPath}.`);
}
const password = process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? (fs.existsSync(passwordPath) ? fs.readFileSync(passwordPath, 'utf8').trim() : null);
if (password === null) fail(`Key password not found: save it in ${passwordPath} or set TAURI_SIGNING_PRIVATE_KEY_PASSWORD.`);
const env = {
  ...process.env,
  TAURI_SIGNING_PRIVATE_KEY: process.env.TAURI_SIGNING_PRIVATE_KEY || keyPath,
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD: password,
};

if (process.argv.includes('--check')) {
  console.log(`Pre-flight OK: version ${version} everywhere, changelog section found (${notes.split('\n').length} lines), signing key and password present.`);
  process.exit(0);
}

console.log(`Building Mineger ${version} (signed)…`);
const build = spawnSync('npm run tauri -- build', { stdio: 'inherit', env, shell: true, cwd: root });
if (build.status !== 0) process.exit(build.status ?? 1);

const bundle = path.join(root, 'src-tauri/target/release/bundle');
const find = (dir, test) => {
  const hit = fs.existsSync(dir) ? fs.readdirSync(dir).find(test) : null;
  return hit ? path.join(dir, hit) : null;
};
// Older builds stay in the bundle folder: pick the files of this version only.
const setup = find(path.join(bundle, 'nsis'), (f) => f.includes(`_${version}_`) && f.endsWith('-setup.exe'));
const msi = find(path.join(bundle, 'msi'), (f) => f.includes(`_${version}_`) && f.endsWith('.msi'));
if (!setup) fail(`NSIS installer for ${version} not found under bundle/nsis`);
if (!fs.existsSync(`${setup}.sig`)) fail(`${setup}.sig missing: is bundle.createUpdaterArtifacts true in tauri.conf.json?`);

const manifest = {
  version,
  notes,
  pub_date: new Date().toISOString(),
  platforms: {
    'windows-x86_64': {
      signature: fs.readFileSync(`${setup}.sig`, 'utf8').trim(),
      url: `https://github.com/${REPO}/releases/download/v${version}/${path.basename(setup)}`,
    },
  },
};
const manifestPath = path.join(bundle, 'latest.json');
fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n');

const notesPath = path.join(bundle, `release-notes-${version}.md`);
fs.writeFileSync(notesPath, notes + '\n');

console.log(`
Signed installer: ${setup}
MSI:              ${msi ?? '(none)'}
Manifest:         ${manifestPath}
Release notes:    ${notesPath}

Publish with:
  gh release create v${version} --title "Mineger ${version}" --notes-file "${notesPath}" "${setup}"${msi ? ` "${msi}"` : ''} "${manifestPath}"

The manifest must be attached to the release as latest.json: running apps read
https://github.com/${REPO}/releases/latest/download/latest.json
`);
