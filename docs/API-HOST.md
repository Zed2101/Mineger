# Host API and webhooks

*[Leggi questo file in italiano](API-HOST.it.md)*

> Prefer a browsable version? Every endpoint, with an example and a use case, is on the website: **[zed2101.github.io/Mineger/api.html](https://zed2101.github.io/Mineger/api.html)**.

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
| `GET` | `/api/servers/{id}/commands` | Commands the server accepts, for autocompletion: `{version, taken_at, commands: [{name, usage, alias_of?}]}`; `null` until the first start (the snapshot is taken from `help` when the server comes online) |
| `GET` | `/api/servers/{id}/commands/{name}` | Every form of one command (`help <name>`, server running): `["/tp <destination>", "/tp <targets> <location>", …]` |
| `GET` | `/api/servers/{id}/network` | The three ways in, in one answer: `{lan_ip, port, running, upnp_enabled, upnp_state, upnp_message?, public_ip?, upnp_cgnat, tunnel}` (`upnp_state`: `off`, `idle`, `opening`, `open`, `failed`; `tunnel` as below). Toggling `upnp`/`tunnel` through `PUT /launch` applies immediately when the server runs; the `network-status` event follows UPnP changes |
| `POST` | `/api/servers/{id}/tunnel/retry` | Retries the playit tunnel of a running server after an error (same as the Retry button) |
| `GET` | `/api/servers/{id}/tunnel` | playit.gg tunnel of the server: `{linked, account?, enabled, agent_running, state, address?, message?}` (`state`: `off`, `starting`, `online`, `error`). Linking the account is local to the host app |
| `GET` | `/api/servers/{id}/metrics` | CPU and RAM of the Java process |
| `GET` | `/api/servers/{id}/disk-usage` | Bytes and file count of the folder |

### Map and players

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/servers/{id}/map` | Dimensions with their regions, spawn, saved players (position, dimension, online flag) |
| `GET` | `/api/servers/{id}/map/tile/{dim}/{rx}/{rz}` | PNG tile of one region as `{ "png": "<base64>" }`; `?force=true` re-renders it |
| `POST` | `/api/servers/{id}/map/render` | Re-render the stale tiles of a dimension: `{dimension, force}`; progress in the `map-progress` event |
| `POST` | `/api/servers/{id}/map/search` | Search every dimension: `{query, dimension, x, z}` → `[{kind, id, label, dimension, x, y, z, distance}]` (kinds: `player`, `coords`, `structure`, `biome`, `poi`, `entity`, `sign`) |
| `GET` | `/api/servers/{id}/players/live` | Position and dimension of the online players (asked to the server with `/data get entity`) |
| `GET` | `/api/servers/{id}/players/{name}/inventory` | Inventory of an online player: `[{slot, id, count}]` |

Tiles are rendered from the region files, so the map works with the server off and for every server kind; they are cached in `<server>/.mineger/map/`.

### Configuration

| Method | Path | Description |
|---|---|---|
| `PUT` | `/api/servers/{id}/info` | Name and icon: `{name, icon}` |
| `PUT` | `/api/servers/{id}/launch` | RAM, UPnP and playit tunnel: `{max_ram_mb, upnp, tunnel}` |
| `PUT` | `/api/servers/{id}/properties` | `server.properties`: `{properties: {...}}` |
| `GET`/`PUT`/`DELETE` | `/api/servers/{id}/server-icon` | Server icon (64×64 PNG, resized by the app) |
| `GET`/`POST` | `/api/servers/{id}/backups` | List or create a world backup (retention and the `backup-result` event apply to every backup) |
| `GET` | `/api/servers/{id}/backups/stats` | `{count, bytes, last_backup?}`: how many backups, the space they take, and the outcome of the last one (`{at, ok, source, file?, error?}`; `source`: `manual`, `schedule`, `on_stop`, `pre_restore`) |
| `GET` | `/api/servers/{id}/backups/{file}` | Preview of a backup: `{file, entries, bytes, worlds, has_level_dat}` |
| `DELETE` | `/api/servers/{id}/backups/{file}` | Deletes one backup file |
| `POST` | `/api/servers/{id}/backups/restore` | `{file, safety?}`: restores the backup over the server's world folders. Server must be off; with `safety` (default `true`) a backup of the current world is taken first. Returns `{restored_files, safety_backup?}` |
| `GET`/`PUT` | `/api/servers/{id}/automation` | The server's automation: `{restart: {enabled, max_attempts, window_minutes}, schedules: [{id, action, command, when, enabled, warn_minutes, last_run?, last_ok?, last_result?}], backup: {keep_last?, keep_days?, on_stop}, discord: {enabled, url, on_start, on_stop, on_crash, on_backup_failed, on_backup_done, on_join, on_leave, on_schedule}, last_backup?}`. `action`: `start`, `stop`, `restart`, `backup`, `command`; `when`: `{kind: "daily", time: "04:00"}`, `{kind: "weekly", days: [0..6], time}` (0 = Monday) or `{kind: "interval", minutes}` (5 minimum). `PUT` sends the whole object (an empty `id` gets one assigned); run outcomes are kept server-side |
| `POST` | `/api/servers/{id}/automation/run` | `{schedule_id}`: runs a schedule now. Outcome as the `schedule-run` event |
| `POST` | `/api/servers/{id}/automation/discord/test` | `{url}`: sends a test message to a Discord webhook |

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
| `GET` | `/api/servers/{id}/mods/sides` | Client/server side of every installed mod (`both` · `client` · `server` · `unknown`, with where it comes from: `api` · `jar` · `list`), loader and Minecraft version: what friends must install, which client-only mods are pointless on the server |

`provider`: `modrinth` · `curseforge`. Searches always filter by the **server's loader**; when nothing exists for the Minecraft version, the response carries `relaxed_mc: true` and the builds are marked `compatible: false`.

### Modpacks

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/packs/resolve` | Read a modpack link: `{url}` |
| `POST` | `/api/packs/install` | Install: `{name, provider, project_id, file_id}` |
| `GET` | `/api/servers/{id}/updates` | Check for modpack updates |
| `POST` | `/api/servers/{id}/update` | Update the modpack (backup + data migration) |
| `GET` | `/api/servers/{id}/rollback` | Previous modpack version kept next to the server after an update (`null` if none) |
| `POST` | `/api/servers/{id}/rollback` | Go back to the previous modpack version (server off): world, settings and backups are kept, the undone version stays in `.undone-…` |

`/api/packs/resolve` answers `400` with `error` starting with `LINK_KIND:<kind>:` (`mod` · `plugin` · `resourcepack` · `datapack` · `shader` · `world` · `unknown`) when the link is not a modpack; the text after the second colon explains where that content goes. `/api/servers/{id}/update` answers with `kept`, `replaced`, `backup_file` and `previous_version` next to `new_version`. Rate limits from CurseForge and Modrinth are retried with growing waits; the progress events carry phase `wait` meanwhile.

### Real-time events

```
GET /api/ws?token=<token>
```

WebSocket that forwards the app's events: `server-status`, `server-output`, `create-progress`, `update-progress`, `mod-progress`, `backup-progress`, `pack-updates`, `webhook-call`, `map-progress`, `commands-ready`, `tunnel-status`, `network-status`, `backup-result` (`{id, ok, source, file?, error?, at, pruned}` after every backup, whoever started it), `schedule-run` (`{id, schedule_id, ok, message, at}`).

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
