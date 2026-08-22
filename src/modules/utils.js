// src/modules/utils.js

import { t, currentLanguage } from './i18n.js';

export function formatBytes(bytes, decimals = 1) {
  if (!+bytes) return '0 B';
  const k = 1024;
  const dm = decimals < 0 ? 0 : decimals;
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${parseFloat((bytes / Math.pow(k, i)).toFixed(dm))} ${sizes[i]}`;
}

/** "18.4 / 50 GB" */
export function formatGB(bytes, decimals = 1) {
  return (bytes / 1024 ** 3).toFixed(decimals);
}

/** Escape per inserire testo non fidato (nomi file/server) dentro innerHTML. */
export function escapeHtml(value) {
  return String(value ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/** 8_040_000 ms → "2h 14m"; 45_000 → "45s"; 3 giorni → "3d 2h" */
export function formatUptime(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return t('msg2.time.uptime_dh', { d, h });
  if (h > 0) return t('msg2.time.uptime_hm', { h, m });
  if (m > 0) return t('msg2.time.uptime_m', { m });
  return t('msg2.time.uptime_s', { s });
}

/** epoch ms → "08:38" */
export function formatClock(ms) {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

/** epoch ms → "oggi 09:12" | "ieri 21:03" | "12/08 09:12" */
export function formatRelativeDay(ms) {
  const d = new Date(ms);
  const now = new Date();
  const sameDay = (a, b) => a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  const clock = formatClock(ms);
  if (sameDay(d, now)) return t('msg2.time.today', { clock });
  if (sameDay(d, yesterday)) return t('msg2.time.yesterday', { clock });
  return t('msg2.time.date_short', {
    day: String(d.getDate()).padStart(2, '0'),
    month: String(d.getMonth() + 1).padStart(2, '0'),
    clock,
  });
}

/** "xLuca_ITA" → "XL" */
export function initials(name) {
  const clean = String(name).replace(/[^a-zA-Z0-9]/g, '');
  return (clean.slice(0, 2) || '??').toUpperCase();
}

const AVATAR_CLASSES = [
  'bg-emerald-500/25 text-emerald-300',
  'bg-sky-500/25 text-sky-300',
  'bg-amber-500/25 text-amber-300',
  'bg-rose-500/25 text-rose-300',
  'bg-violet-500/25 text-violet-300',
  'bg-teal-500/25 text-teal-300',
];

/** Colore avatar deterministico per nome giocatore */
export function avatarClass(name) {
  let h = 0;
  for (const c of String(name)) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return AVATAR_CLASSES[h % AVATAR_CLASSES.length];
}

/** 2418 → "2.418" (it) / "2,418" (en) */
export function formatInt(n) {
  return Number(n || 0).toLocaleString(currentLanguage());
}
