// src/modules/api.js
//
// Strato di instradamento: i comandi che riguardano un server vanno al backend
// locale (`invoke`) oppure, se l'id è `remote:<hostId>:<serverId>`, all'host
// remoto registrato (HTTP/WebSocket, vedi remote.js). La UI non deve saperlo.

import { t } from './i18n.js';

const { invoke } = window.__TAURI__.core;

const remoteHosts = new Map(); // hostId -> RemoteHost

export const REMOTE_PREFIX = 'remote:';

export function isRemoteId(id) {
  return typeof id === 'string' && id.startsWith(REMOTE_PREFIX);
}

/** `remote:<hostId>:<serverId>` → { hostId, serverId } (il serverId può contenere ':') */
export function splitRemoteId(id) {
  const rest = id.slice(REMOTE_PREFIX.length);
  const sep = rest.indexOf(':');
  return { hostId: rest.slice(0, sep), serverId: rest.slice(sep + 1) };
}

export function makeRemoteId(hostId, serverId) {
  return `${REMOTE_PREFIX}${hostId}:${serverId}`;
}

export function registerRemoteHost(hostId, client) {
  remoteHosts.set(hostId, client);
}

export function unregisterRemoteHost(hostId) {
  remoteHosts.delete(hostId);
}

export function getRemoteHost(hostId) {
  return remoteHosts.get(hostId);
}

/** Esegue un comando; se `args.id` è remoto lo inoltra all'host giusto. */
export async function call(cmd, args = {}) {
  const id = args.id;
  if (!isRemoteId(id)) return invoke(cmd, args);

  const { hostId, serverId } = splitRemoteId(id);
  const host = remoteHosts.get(hostId);
  if (!host) throw t('msg2.remote.not_connected');
  return host.call(cmd, { ...args, id: serverId });
}

export async function getServers() {
  return invoke('get_servers');
}
