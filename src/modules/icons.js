// src/modules/icons.js
//
// Icone dei server: quelle predefinite si caricano da ./assets/<nome>, quelle
// caricate dall'utente arrivano dal backend come data URL e restano in cache.

const { invoke } = window.__TAURI__.core;

export const DEFAULT_ICON = 'mc-img.jpg';

let icons = [];                 // IconInfo[] { name, builtin, data_url, size }
const dataUrls = new Map();     // name -> data URL (solo icone utente)
let loaded = false;

export async function loadIcons(force = false) {
  if (loaded && !force) return icons;
  try {
    icons = await invoke('list_icons');
  } catch (err) {
    console.warn('list_icons', err);
    icons = [{ name: DEFAULT_ICON, builtin: true, data_url: null, size: 0 }];
  }
  dataUrls.clear();
  for (const i of icons) if (i.data_url) dataUrls.set(i.name, i.data_url);
  loaded = true;
  return icons;
}

export function allIcons() {
  return icons;
}

/** URL usabile in <img src> per il nome icona salvato nel server. */
export function iconUrl(name) {
  if (!name) return `./assets/${DEFAULT_ICON}`;
  return dataUrls.get(name) || `./assets/${encodeURIComponent(name)}`;
}

/** Aggiunge/aggiorna un'icona appena importata. */
export function rememberIcon(info) {
  if (!info) return;
  if (info.data_url) dataUrls.set(info.name, info.data_url);
  const idx = icons.findIndex((i) => i.name === info.name);
  if (idx >= 0) icons[idx] = info;
  else icons.push(info);
}

export function forgetIcon(name) {
  dataUrls.delete(name);
  icons = icons.filter((i) => i.name !== name);
}
