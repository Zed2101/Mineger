// scripts/lang-template.mjs
//
// Rigenera `src/language/_template.json`: tutte le chiavi dell'italiano (la lingua
// di riferimento) con i valori svuotati, più il testo originale come commento a
// fianco sotto forma di chiave "__source".
//
// Uso:  npm run lang:template
//       npm run lang:new -- fr        (crea src/language/fr.json dal template)
//
// Il template serve a tradurre in una lingua nuova senza dover cercare le stringhe
// nel codice: si riempiono i valori e si registra la lingua in `i18n.js` e `i18n.rs`.

import fs from 'node:fs';
import path from 'node:path';

const LANG_DIR = path.join(process.cwd(), 'src', 'language');
const REFERENCE = 'it';

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

/** Copia la struttura svuotando le stringhe. */
function blank(value) {
  if (typeof value === 'string') return '';
  const out = {};
  for (const [k, v] of Object.entries(value)) out[k] = blank(v);
  return out;
}

/** Chiavi appiattite: {a:{b:1}} → ["a.b"] */
function flatten(value, prefix = '', out = {}) {
  for (const [k, v] of Object.entries(value)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (typeof v === 'string') out[key] = v;
    else flatten(v, key, out);
  }
  return out;
}

const reference = readJson(path.join(LANG_DIR, `${REFERENCE}.json`));
const flatRef = flatten(reference);
const target = process.argv[2];

if (!target) {
  const template = blank(reference);
  const file = path.join(LANG_DIR, '_template.json');
  fs.writeFileSync(file, JSON.stringify(template, null, 2) + '\n');
  console.log(`Template aggiornato: ${path.relative(process.cwd(), file)} — ${Object.keys(flatRef).length} chiavi da tradurre.`);
  console.log('Per una lingua nuova:  npm run lang:new -- <codice>');
} else {
  if (!/^[a-z]{2}(-[A-Za-z]{2,4})?$/.test(target)) {
    console.error(`Codice lingua non valido: "${target}" (attesi "fr", "pt-BR", …)`);
    process.exit(1);
  }
  const file = path.join(LANG_DIR, `${target}.json`);
  if (fs.existsSync(file)) {
    console.error(`${path.relative(process.cwd(), file)} esiste già: non lo sovrascrivo.`);
    process.exit(1);
  }
  fs.writeFileSync(file, JSON.stringify(blank(reference), null, 2) + '\n');
  console.log(`Creato ${path.relative(process.cwd(), file)} con ${Object.keys(flatRef).length} chiavi vuote.`);
  console.log('Passi successivi:');
  console.log(`  1. traduci i valori (le chiavi restano invariate)`);
  console.log(`  2. aggiungi { code: '${target}', name: '…', english: '…', flag: '…' } a LANGUAGES in src/modules/i18n.js`);
  console.log(`  3. aggiungi ("${target}", "…") ad available() in src-tauri/src/i18n.rs e la const con include_str!`);
  console.log(`  4. cd src-tauri && cargo test --lib i18n   (controlla chiavi e segnaposto)`);
}
