// src/modules/links.js
//
// Riconoscimento lato client dei link CurseForge / Modrinth / FTB, specchio di
// `providers::parse_link` nel backend: serve al catalogo mod (un link modpack
// incollato nella ricerca) e al wizard (errore `LINK_KIND:<kind>:…`).

/** Prefisso dell'errore strutturato del backend per i link che non sono modpack. */
export const LINK_KIND_PREFIX = 'LINK_KIND:';

const CF_CLASSES = {
  modpacks: 'modpack',
  'mc-mods': 'mod',
  'bukkit-plugins': 'plugin',
  'texture-packs': 'resourcepack',
  'data-packs': 'datapack',
  shaders: 'shader',
  worlds: 'world',
};

const MR_TYPES = {
  modpack: 'modpack',
  mod: 'mod',
  plugin: 'plugin',
  datapack: 'datapack',
  resourcepack: 'resourcepack',
  shader: 'shader',
  project: 'unknown',
};

/** Sembra un URL (con o senza schema)? */
export function looksLikeUrl(text) {
  const s = String(text || '').trim();
  return /^(https?:\/\/)?(www\.)?[a-z0-9-]+(\.[a-z0-9-]+)+\//i.test(s);
}

/**
 * Classifica un link. Ritorna `null` se non è un link di CurseForge, Modrinth o FTB,
 * altrimenti `{ provider, kind, slug, fileId }` con kind ∈ modpack · mod · plugin ·
 * resourcepack · datapack · shader · world · unknown.
 */
export function classifyLink(text) {
  let s = String(text || '').trim();
  if (!s) return null;
  s = s.replace(/^https?:\/\//i, '').replace(/^www\./i, '');
  const path = s.split(/[?#]/)[0].split('/').filter(Boolean);
  const host = (path[0] || '').toLowerCase();
  const numeric = (x) => /^\d+$/.test(x || '');

  if (host.endsWith('curseforge.com')) {
    if (path.length >= 4 && path[1] === 'minecraft') {
      const kind = CF_CLASSES[path[2]] || 'unknown';
      const fileId = (path[4] === 'files' || path[4] === 'download') && numeric(path[5]) ? path[5] : null;
      return { provider: 'curseforge', kind, slug: path[3], fileId };
    }
    if (path.length >= 3 && path[1] === 'projects' && numeric(path[2])) {
      return { provider: 'curseforge', kind: 'unknown', slug: path[2], fileId: null };
    }
    return null;
  }

  if (host.endsWith('modrinth.com')) {
    if (path.length >= 3 && MR_TYPES[path[1]]) {
      const fileId = path[3] === 'version' && path[4] ? path[4] : null;
      return { provider: 'modrinth', kind: MR_TYPES[path[1]], slug: path[2], fileId };
    }
    return null;
  }

  if (host.endsWith('feed-the-beast.com') || host.endsWith('modpacks.ch')) {
    const i = path.findIndex((p) => p === 'modpacks' || p === 'modpack');
    if (i >= 0 && path[i + 1]) {
      const id = (path[i + 1].match(/^\d+/) || [path[i + 1]])[0];
      return { provider: 'ftb', kind: 'modpack', slug: id, fileId: null };
    }
    return null;
  }

  return null;
}

/**
 * `LINK_KIND:<kind>:<messaggio>` → `{ kind, message }`, altrimenti `null`.
 */
export function splitLinkKindError(err) {
  const text = String(err ?? '');
  if (!text.startsWith(LINK_KIND_PREFIX)) return null;
  const rest = text.slice(LINK_KIND_PREFIX.length);
  const sep = rest.indexOf(':');
  if (sep < 0) return null;
  return { kind: rest.slice(0, sep), message: rest.slice(sep + 1) };
}
