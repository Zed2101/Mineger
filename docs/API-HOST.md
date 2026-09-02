# Host API and webhooks

*[Leggi questo file in italiano](API-HOST.it.md)*

A Mineger installation can act as a **host**: it exposes its servers on the network, so other copies of the app (or any HTTP client) can manage them remotely. Enable it under **Settings → Remote control**.

The host serves two different things:

- **Control API** under `/api/...`, protected by a token — the one the remote app uses.
- **Webhooks** under `/hook/<id>`, public but with their own token and permissions — meant for Discord bots, Twitch extensions, scripts.

Default port: **25580**.

---

## Authentication

Every request to `/api/...` requires the host token:

```
Authorization: Bearer <token>
```

The invite link the app generates (`mineger://…`) contains address, port and token: paste it into *Connect remote* and the client configures itself.

> The token grants **full control** over the host's servers: share it only with people you want administering them.

The token is accepted **only in the header**. The single exception is `/api/ws?token=…`, because a browser cannot set headers on a WebSocket: there the token travels in the query string.

### Limits and protections

| What | Value |
|---|---|
| Maximum body on `/api/...` | 1 MB (16 MB for the server icon, 1 GB only for mod uploads) |
| Maximum body on `/hook/{id}` | 64 KB |
| Failed authentication attempts | 20 per minute per address, then `429 Too Many Requests` |
| Token comparison | constant-time |
| CORS | only the Mineger webview origins (non-browser clients are unaffected) |
| Listen address | **Settings → Remote control → Listen on**: whole network (default) or this PC only |

---

## Endpoints

### General

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/info` | Host name, version, disk space |
| `GET` | `/api/servers` | Full server list with state, mods, properties |
| `POST` | `/api/servers/create` | Create a server: `{name, kind, mc_version, loader_version}` |
| `DELETE` | `/api/servers/{id}` | Permanently delete a server (refused while it is running) |
| `POST` | `/api/loaders/mc-versions` | Minecraft versions for a kind: `{kind}` |
| `POST` | `/api/loaders/versions` | Loader builds: `{kind, mc_version}` |

`kind`: `vanilla` · `paper` · `forge` · `neoforge` · `fabric`.

### Server lifecycle

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/servers/{id}/start` · `/stop` · `/kill` | Start, stop with `stop`, kill the process |
| `POST` | `/api/servers/{id}/command` | Send a command to the console: `{command}` |
| `POST` | `/api/servers/{id}/eula` | Accept the Minecraft EULA |
| `GET` | `/api/servers/{id}/logs` | Latest console lines |
| `GET` | `/api/servers/{id}/metrics` | CPU and RAM of the Java process |
| `GET` | `/api/servers/{id}/disk-usage` | Bytes and file count of the folder |

### Configuration

| Method | Path | Description |
|---|---|---|
| `PUT` | `/api/servers/{id}/info` | Name and icon: `{name, icon}` |
| `PUT` | `/api/servers/{id}/launch` | RAM and UPnP: `{max_ram_mb, upnp}` |
| `PUT` | `/api/servers/{id}/properties` | `server.properties`: `{properties: {...}}` |
| `GET`/`PUT`/`DELETE` | `/api/servers/{id}/server-icon` | Server icon (64×64 PNG, resized by the app) |
| `GET`/`POST` | `/api/servers/{id}/backups` | List or create a world backup |

### Mods and plugins

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/servers/{id}/content` | Context: server kind, folder, version, loader, whether CurseForge is configured |
| `POST` | `/api/servers/{id}/content/search` | Search: `{provider, query, limit}` |
| `POST` | `/api/servers/{id}/content/versions` | Versions of a project: `{provider, project_id}` |
| `POST` | `/api/servers/{id}/content/install` | Install: `{provider, project_id, file_id}` |
| `GET` | `/api/servers/{id}/content/updates` | Updates available for the installed mods |
| `POST` | `/api/servers/{id}/content/update` | Update one mod: `{name}` |
| `POST` | `/api/servers/{id}/mods` | Upload `.jar` files (multipart) |
| `POST` | `/api/servers/{id}/mods/toggle` | Enable/disable: `{name, enabled}` |
| `DELETE` | `/api/servers/{id}/mods/{name}` | Delete a file |

`provider`: `modrinth` · `curseforge`. Searches always filter by the **server's loader**; when nothing exists for the Minecraft version, the response carries `relaxed_mc: true` and the builds are marked `compatible: false`.

### Modpacks

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/packs/resolve` | Read a modpack link: `{url}` |
| `POST` | `/api/packs/install` | Install: `{name, provider, project_id, file_id}` |
| `GET` | `/api/servers/{id}/updates` | Check for modpack updates |
| `POST` | `/api/servers/{id}/update` | Update the modpack (backup + data migration) |

### Real-time events

```
GET /api/ws?token=<token>
```

WebSocket that forwards the app's events: `server-status`, `server-output`, `create-progress`, `update-progress`, `mod-progress`, `backup-progress`, `pack-updates`, `webhook-call`.

---

## Webhooks

Every server has a **Webhook** tab where you can create as many as you need. Each webhook has its own id, token and permissions.

```
GET  /hook/<id>?token=<token>&action=say&message=Hello
POST /hook/<id>          { "token": "...", "action": "command", "command": "time set day" }
```

Parameters can be passed as query string, JSON or form: handy for services that can only send a GET.

### Actions and permissions

| Action | Parameters | Permission |
|---|---|---|
| `say` | `message` | Messages |
| `command` | `command` | Commands |
| `start` / `stop` | — | Power |
| `status` | — | Status |

Rules enforced by the host:

- Every action requires the **matching permission**, each one enabled individually.
- Commands are restricted to an **allow list** you define (empty = every command allowed by the "Commands" permission).
- The `stop` command is **always refused** by the `command` action: stopping the server requires the explicit power permission.
- Every call is recorded (time, outcome, IP) and shown in the Webhook tab.

### Example: Discord bot

```js
await fetch(`http://lukes-pc:25580/hook/${id}`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ token, action: 'say', message: `<${user}> ${text}` }),
});
```

### Example: from the terminal

```bash
curl "http://127.0.0.1:25580/hook/ID?token=TOKEN&action=status"
```

---

## Network notes

- By default the host listens on every interface: to be reachable from outside your home it needs a port forward or the router's UPnP. If access goes through a tunnel or a VPN on the same machine, choose **This PC only** in the settings.
- Traffic is **plain HTTP**: fine for a local network or a VPN among friends. Don't expose the host on the Internet without a TLS reverse proxy.
- Whoever connects sees and controls only the host's servers, not the rest of the computer.
