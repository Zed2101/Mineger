// src/modules/remote.js
//
// Client verso un Mineger in modalità host: REST con bearer token per i comandi,
// WebSocket per gli eventi (server-output, server-status, backup-progress) con
// riconnessione automatica.

const RECONNECT_MIN_MS = 2000;
const RECONNECT_MAX_MS = 30000;
const FETCH_TIMEOUT_MS = 20000;

const enc = (s) => encodeURIComponent(s);

export class RemoteHost {
  /**
   * @param meta     { id, name, url, token }
   * @param handlers { onEvent(type, payload), onConnection(connected, error) }
   */
  constructor(meta, handlers) {
    this.meta = meta;
    this.handlers = handlers;
    this.ws = null;
    this.closed = false;
    this.connected = false;
    this.backoff = RECONNECT_MIN_MS;
    this.reconnectTimer = null;
  }

  get url() {
    return this.meta.url.replace(/\/+$/, '');
  }

  get wsUrl() {
    return this.url.replace(/^http/, 'ws') + `/api/ws?token=${enc(this.meta.token)}`;
  }

  // ---------------------------------------------------------------------
  // HTTP
  // ---------------------------------------------------------------------

  async request(method, path, { json, formData } = {}) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    const headers = { Authorization: `Bearer ${this.meta.token}` };
    let body;
    if (json !== undefined) {
      headers['Content-Type'] = 'application/json';
      body = JSON.stringify(json);
    } else if (formData) {
      body = formData;
    }

    let res;
    try {
      res = await fetch(`${this.url}${path}`, { method, headers, body, signal: controller.signal });
    } catch (err) {
      throw `Host "${this.meta.name}" non raggiungibile (${err?.message || err})`;
    } finally {
      clearTimeout(timer);
    }

    const text = await res.text();
    let data = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      data = text;
    }

    if (!res.ok) {
      if (data && typeof data === 'object' && data.error) throw String(data.error);
      if (res.status === 401) throw 'Token rifiutato dall\'host';
      throw `HTTP ${res.status}`;
    }
    return data;
  }

  /** Mappa i comandi Tauri sugli endpoint REST dell'host. */
  async call(cmd, args = {}) {
    const id = args.id !== undefined ? enc(args.id) : null;
    const s = (suffix = '') => `/api/servers/${id}${suffix}`;

    switch (cmd) {
      case 'get_servers':
        return this.request('GET', '/api/servers');
      case 'get_recent_logs':
        return this.request('GET', s('/logs'));
      case 'start_server':
        return this.request('POST', s('/start'));
      case 'stop_server':
        return this.request('POST', s('/stop'));
      case 'kill_server':
        return this.request('POST', s('/kill'));
      case 'send_command':
        return this.request('POST', s('/command'), { json: { command: args.command } });
      case 'accept_eula_cmd':
        return this.request('POST', s('/eula'));
      case 'get_server_metrics':
        return this.request('GET', s('/metrics'));
      case 'list_backups':
        return this.request('GET', s('/backups'));
      case 'create_backup':
        return this.request('POST', s('/backups'));
      case 'delete_server':
        return this.request('DELETE', s(''));
      case 'get_server_disk_usage':
        return this.request('GET', s('/disk-usage'));
      case 'create_server':
        return this.request('POST', '/api/servers/create', {
          json: { name: args.name, kind: args.kind, mc_version: args.mcVersion, loader_version: args.loaderVersion ?? null },
        }).then((r) => r.id);
      case 'get_mc_versions':
        return this.request('POST', '/api/loaders/mc-versions', { json: { kind: args.kind } });
      case 'get_loader_versions':
        return this.request('POST', '/api/loaders/versions', { json: { kind: args.kind, mc_version: args.mcVersion } });
      case 'get_mod_context':
        return this.request('GET', s('/content'));
      case 'search_mods':
        return this.request('POST', s('/content/search'), { json: { provider: args.provider, query: args.query, limit: args.limit ?? 20 } });
      case 'get_mod_versions':
        return this.request('POST', s('/content/versions'), { json: { provider: args.provider, project_id: args.projectId } });
      case 'install_mod':
        return this.request('POST', s('/content/install'), { json: { provider: args.provider, project_id: args.projectId, file_id: args.fileId } });
      case 'check_mod_updates':
        return this.request('GET', s('/content/updates'));
      case 'update_mod':
        return this.request('POST', s('/content/update'), { json: { name: args.name } });
      case 'update_server_info':
        return this.request('PUT', s('/info'), { json: { name: args.name, icon: args.icon ?? null } });
      case 'update_launch_config':
        return this.request('PUT', s('/launch'), { json: { max_ram_mb: args.maxRamMb ?? null, upnp: args.upnp ?? null } });
      case 'save_server_properties':
        return this.request('PUT', s('/properties'), { json: { properties: args.properties } });
      case 'toggle_mod':
        return this.request('POST', s('/mods/toggle'), { json: { name: args.name, enabled: args.enabled } });
      case 'delete_mod':
        return this.request('DELETE', s(`/mods/${enc(args.name)}`));
      case 'add_mods': {
        const files = args.files || [];
        if (files.length === 0) {
          const mods = await this.request('GET', '/api/servers').then((list) => list.find((x) => x.id === args.id)?.mods ?? []);
          return { mods, added: 0, skipped: [] };
        }
        const fd = new FormData();
        for (const f of files) fd.append('file', f, f.name);
        return this.request('POST', s('/mods'), { formData: fd });
      }
      case 'get_server_icon':
        return this.request('GET', s('/server-icon'));
      case 'set_server_icon_from_bytes':
        return this.request('PUT', s('/server-icon'), { json: { data_base64: args.dataBase64 } });
      case 'remove_server_icon':
        return this.request('DELETE', s('/server-icon'));
      case 'open_server_folder':
      case 'set_server_icon_from_path':
      case 'pick_server_icon_file':
        throw 'Non disponibile per i server remoti';
      default:
        throw `Comando non supportato da remoto: ${cmd}`;
    }
  }

  // ---------------------------------------------------------------------
  // WebSocket eventi
  // ---------------------------------------------------------------------

  connect() {
    this.closed = false;
    this.open();
  }

  open() {
    if (this.closed) return;
    try {
      this.ws = new WebSocket(this.wsUrl);
    } catch (err) {
      this.scheduleReconnect(String(err));
      return;
    }

    this.ws.onopen = () => {
      this.connected = true;
      this.backoff = RECONNECT_MIN_MS;
      this.handlers.onConnection?.(true, null);
    };

    this.ws.onmessage = (msg) => {
      try {
        const { type, payload } = JSON.parse(msg.data);
        this.handlers.onEvent?.(type, payload);
      } catch (err) {
        console.warn('remote event non valido', err);
      }
    };

    this.ws.onerror = () => {
      /* onclose segue sempre */
    };

    this.ws.onclose = () => {
      const wasConnected = this.connected;
      this.connected = false;
      this.ws = null;
      if (wasConnected || !this.closed) this.handlers.onConnection?.(false, 'connessione persa');
      this.scheduleReconnect();
    };
  }

  scheduleReconnect() {
    if (this.closed || this.reconnectTimer) return;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.open();
    }, this.backoff);
    this.backoff = Math.min(this.backoff * 2, RECONNECT_MAX_MS);
  }

  close() {
    this.closed = true;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.ws) {
      try {
        this.ws.close();
      } catch {
        /* ignore */
      }
      this.ws = null;
    }
    this.connected = false;
  }
}
