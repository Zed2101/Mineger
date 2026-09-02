# Architecture

*[Leggi questo file in italiano](ARCHITECTURE.it.md)*

Mineger is a [Tauri v2](https://tauri.app) app: Rust backend, HTML/CSS/JS frontend with no framework and no bundler (native ES modules), Tailwind CSS v4 for styling.

```
src/                    frontend (served by Tauri as frontendDist)
  index.html            the whole UI: sidebar, tabs, modals
  app.css → tailwind.css  Tailwind source and generated output (gitignored)
  main.js               startup, global state, routing between views
  modules/              one module per functional area
src-tauri/
  src/                  Rust backend
  tauri.conf.json       window, bundle, permissions
```

## Core principle

The logic lives in **`service.rs`** and the domain modules, independent of the transport. On top of it there are only two façades:

- **`commands.rs`** — Tauri commands called by the local frontend (`invoke`).
- **`host.rs`** — [axum](https://github.com/tokio-rs/axum) HTTP/WebSocket server for remote clients.

Both call the same functions: a new feature is written once and works both locally and remotely.

## Backend modules

| Module | Responsibility |
|---|---|
| `service.rs` | Server list, start/stop, mods, properties, backups, icons, deletion |
| `process.rs` | Java processes: spawn, stdin, log reading (tolerant of non-UTF-8 encodings), exit detection, state |
| `launch.rs` | How to start a server: `server.jar`, Forge/NeoForge argument files, `user_jvm_args.txt`, RAM |
| `java.rs` | Detection of installed Java runtimes and choice of the right version for a Minecraft release |
| `create.rs` | Vanilla creation (Mojang manifest), import from ZIP, recognition of imported servers |
| `loaders.rs` | Server kinds and official version lists: Mojang, PaperMC, Forge, NeoForge, Fabric; server creation |
| `packs.rs` | Modpacks: installation, periodic update checks, user data migration, rollback |
| `providers/` | Platform clients: `curseforge`, `modrinth`, `ftb`, `mods` (individual mods/plugins) |
| `modsvc.rs` | Mod/plugin installation, source registry, per-mod updates |
| `upnp.rs` | Port opening on the router (igd-next), handling of the usual router quirks |
| `host.rs` | REST API + WebSocket, token authentication, public webhooks |
| `settings.rs` | `settings.json`: host, remote hosts, webhooks, server order, CurseForge key |
| `events.rs` | Event bus shared between Tauri and the WebSocket |
| `backup.rs` · `servericon.rs` · `icons.rs` · `metrics.rs` | World backups, 64×64 `server-icon.png`, server icons, CPU/RAM sampling |

## Frontend modules

| Module | Responsibility |
|---|---|
| `api.js` | Routes calls: local ones via `invoke`, remote ones (`remote:<host>:<id>`) via `RemoteHost` |
| `remote.js` | REST + WebSocket client with automatic reconnection |
| `ui-status.js` · `ui-console.js` | Server state, console with log parser |
| `ui-details.js` · `ui-properties.js` | Details screen, `server.properties` editor, UPnP, icon |
| `ui-mods.js` · `ui-modbrowser.js` | Mod/plugin list with source and updates; search and install from the platforms |
| `ui-modals.js` | Server editing, creation wizard, settings, deletion with confirmation |
| `ui-packs.js` | Modpack card, update badges |
| `ui-webhooks.js` | Per-server Webhook tab |
| `sortable.js` · `icons.js` · `ui-tabs.js` · `utils.js` | Drag & drop, icons, tabs, formatting |

## Data on disk

```
servers/<name>/
  server-data.json      name, icon, version, kind, launch options,
                        modpack source, registry of the installed mods
  server.properties     game configuration
  mods/ | plugins/      loadable content (depending on the server kind)
  backups/              world zips
settings.json           app settings, in the app config folder
                        (src-tauri/src/data/ in dev builds, gitignored)
```

`server-data.json` is the app's source of truth; the running state lives only in memory (`process.rs`) and is recomputed at every startup.

## Events

The backend emits events the UI listens to, with no polling:

| Event | When |
|---|---|
| `server-status` | State change (starting/online/stopping/offline) |
| `server-output` | New console line |
| `create-progress` · `update-progress` · `mod-progress` | Progress of creation, modpack update, mod download |
| `pack-updates` | Result of the modpack update check |
| `backup-progress` · `webhook-call` | Backup in progress, webhook received |

The same events travel over the host's WebSocket to remote clients.

## Tests

`cargo test --lib` covers the pure logic: link parsing, loader recognition, versions, safe paths, mod registry, data migration, decoding of API responses (with real samples saved in the repo).

The `#[ignore]` tests do real work — they download official lists, install NeoForge — and run separately with `--ignored`.
