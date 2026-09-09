# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [Unreleased]

### Added
- **"How friends get in" card** in Details: the three ways to reach a server side by side, each with its own switch, state and address: same network (LAN address), router via UPnP (opens at start, closes at stop, public IP shown, CGNAT warning, hint for manual port forwards) and the playit.gg tunnel. They are independent and can all be on. The UPnP and tunnel switches moved there from Properties and apply immediately while the server runs. Host API: `GET /api/servers/{id}/network`, `network-status` event.
- **Tunnel without port forwarding (playit.gg).** Link your own playit.gg account in Settings (approval in the browser); the tunnel switch in the card sends you there when no account is linked. The card offers: link your own playit.gg account (approval in the browser, guest accounts work too), turn the tunnel on per server in Properties, and at the next start the server gets a public address such as `something.joinmc.link:12345` to hand to friends, shown in the card and in the console, the same at every start. Works behind CGNAT, mobile data and routers that refuse UPnP. Mineger runs the official playit agent (`playitd`, BSD-2-Clause, built from source with `npm run playit:build`) as a child process with the user's own key, only while a server with the tunnel is running; only the Minecraft port goes through the tunnel, never the host API. Settings → playit.gg tunnel shows the account and unlinks it. Host API: `tunnel` in `PUT /launch`, `GET /api/servers/{id}/tunnel`, `tunnel-status` event.

- **Automation tab: the server looks after itself.** A new tab per server with four cards:
  - *Automatic restart*: if the Java process exits without being asked (a stop from the app, a webhook or a schedule never counts, nor does a clean exit code 0), Mineger restarts it after growing waits (5 s, 15 s, 45 s, then 60 s), at most N attempts within a window, then gives up and says so. Every step is a console line and, if configured, a Discord message.
  - *Schedules*: start, stop, restart, world backup or a console command, every day at a time, on chosen weekdays, or every N minutes (5 minimum). Each row shows the next run and how the last one went; "Run now" runs it immediately. Stops and restarts can warn the players in chat a few minutes before. Runs missed while the app was closed are not caught up. Times follow the host PC.
  - *Backups*: how many there are and the space they take, the outcome of the last one (a failed backup is red in the tab, in the console and on Discord, never silent), retention (keep the last N and/or drop those older than N days, never the last one left) applied after every backup, manual ones included, an automatic backup at every stop, and the list of backups with **restore**: a preview of the zip (worlds, files, size, whether `level.dat` is there), an explicit confirmation, a safety backup of the current world first, then the world folders are replaced. Restore needs the server off.
  - *Discord notifications*: a channel webhook per server (no bot, no token), with the events to send: start, stop, crash, backup failed, backup done, player joins, player leaves, schedule run. Embeds with the server name; a queue keeps Discord's rate limit; a webhook that does not answer never blocks the server. "Send a test message" checks the URL.
  - The Details backup card links to the tab. Host API: `GET/PUT /api/servers/{id}/automation`, `POST /api/servers/{id}/automation/run`, `POST /api/servers/{id}/automation/discord/test`, `GET /api/servers/{id}/backups/stats`, `GET/DELETE /api/servers/{id}/backups/{file}`, `POST /api/servers/{id}/backups/restore`; events `backup-result` and `schedule-run`. Everything works on remote servers too.
- **"Can your friends get in?"** A button in the "How friends get in" card tests, from outside your network, whether the server port answers on your public IP (portchecker.io does a plain TCP connect to the caller's IP without logging it; api.mcstatus.io does a real Server List Ping to make sure it is *your* server that answers; mcsrvstat.us as fallback; nothing but IP and port ever leaves the PC) and answers in plain language with the cause and the fix when it does not: server off or not listening yet (`server-ip` / `server-port`), router not forwarding the port (UPnP off, refused, or no manual forward), CGNAT / double NAT / DS-Lite (the router's WAN address is private, in 100.64.0.0/10, or differs from the public IP: no port forward can ever work, with a hint for Italian providers and the tunnel as the way that always works), Windows Firewall blocking `java.exe` (the rules are read from the registry without admin rights; "Allow in the firewall" adds the rule through a single UAC prompt, after removing the block rules created by the security alert), the port answering but from a different server, ISP or antivirus blocking. When the playit tunnel is on, its address is tested too and offered as the one that works. The address to share is shown ready to copy, the facts (public IP, router WAN, UPnP state, local listen, external latency, firewall) sit behind "Details", and the suggested fixes are buttons (turn on UPnP, turn on the tunnel, link playit.gg, allow in the firewall, try again). One real test every 10 s per server; works on remote servers too (the test runs on the host). Host API: `POST /api/servers/{id}/reach`, `GET /api/servers/{id}/reach`.

### Changed
- **Settings redesigned.** A wider window with a section index on the left (General, Network, Integrations, System) that scrolls to each section and follows the scroll; every section has an icon, a real title and a one-line description; rows are aligned on a wider label column and the invite links sit in their own box. Detected Java gets a "Download a JDK (OpenLogic)" button and a hint about which Java each Minecraft version needs.

## [1.2.0] — 2026-09-08

### Added
- **World map tab.** A top-down map of the world rendered from the region files, so it works for every server kind and version (vanilla, Paper, Forge, NeoForge, Fabric; 1.2 to 1.21) and with the server off. One tile per region, cached next to the server and re-rendered only when the region changes; modded blocks take their colour from the textures inside the mod jars, modded dimensions are listed next to Overworld, Nether and End. Spawn and players are shown as markers (the skin face from Crafatar with the name above when the server runs in online mode, initials otherwise); with the server online the positions are refreshed every few seconds through `/data get entity`, whose answers are kept out of the console.
- **Player commands from the map.** Clicking a player opens a menu: teleport to coordinates, bring another player here, teleport to another player, view the inventory (read-only), on-screen message, sounds, effects, lightning, game mode, op/de-op, kick. Right-clicking the map teleports a player to that spot (on the surface). "Refresh" saves the world first when the server runs.
- **Map search, across dimensions.** A search box on the map finds players, coordinates (`120 -340`), structures (villages, strongholds, ancient cities, modded ones…), biomes, points of interest (Nether portals, beds, lodestones, villager job sites), named creatures and sign text. Everything comes from what the game already saves (`structures.starts`, section biome palettes, `poi/`, `entities/`, block entities) and is indexed per region next to the map tiles, so a search is instant after the first one. Picking a result switches dimension if needed, centres the map and pulses the spot.
- Host API: `GET /api/servers/{id}/map`, `GET /api/servers/{id}/map/tile/{dim}/{rx}/{rz}`, `POST /api/servers/{id}/map/render`, `POST /api/servers/{id}/map/search`, `GET /api/servers/{id}/players/live`, `GET /api/servers/{id}/players/{name}/inventory`, so the remote app gets the same tab.
- **Console autocompletion.** As soon as a server is online, Mineger asks it `help` (the answer is kept out of the console) and saves the list of commands it accepts, vanilla, mods and plugins alike, in `<server>/.mineger/commands.json`. Typing in the console then suggests command names, fixed choices such as `(survival|creative|adventure|spectator)` and the online players where a `<targets>` is expected; Tab completes, arrows pick, Enter sends. A hint line shows the syntax of the current command (every accepted form, read with `help <name>` while the server runs) with a link to its wiki page.
- **Command reference link** in the console header: opens the Minecraft Wiki commands page (Italian wiki when the app is in Italian), labelled with the server's version.
- Host API: `GET /api/servers/{id}/commands` (snapshot) and `GET /api/servers/{id}/commands/{name}` (every form of one command), plus the `commands-ready` event.
- **In-app updates.** A badge next to the app name in the sidebar appears when a newer release is out (checked at start-up and every 6 hours, or from Settings → Updates). It opens a dialog with the release notes and an "Update now" button: the signed installer is downloaded, its minisign signature verified against the key built into the app, running servers are saved and stopped, then the installer runs and restarts Mineger. At the first start after an update a "What's new" dialog shows the notes for the new version. This is the first release that can update itself: from 1.1.0 and earlier the installer still has to be downloaded by hand once.
- Release tooling: `npm run release:build` builds and signs the installers and writes `latest.json`, the manifest the running apps poll from the latest GitHub release (`plugins.updater` in `tauri.conf.json`, private key outside the repo).

### Changed
- Discord Rich Presence: the site address is shown as text in the idle state too ("Managing 5 servers · zed2101.github.io/Mineger"), since Discord never shows an activity's buttons to its owner.

### Documentation
- **API reference on the website** ([zed2101.github.io/Mineger/api.html](https://zed2101.github.io/Mineger/api.html)): every host endpoint, the WebSocket event stream and the webhooks, grouped by area (host and servers, lifecycle and console, mods and plugins, modpacks, backups, map and players, webhooks), each with a description, a use case, a `curl` request and the response shape. English and Italian, filterable, with a host/token pair that fills the examples without leaving the page. `docs/API-HOST.md` remains the Markdown version; the site's "Docs" link now points to the page.

## [1.1.0] — 2026-09-08

### Added
- **Discord Rich Presence** (Settings → Discord, off by default): your Discord profile shows "Hosting <server> · 3/20 online" with the Mineger logo while a server runs, with a website button for others. Talks to the local Discord client only, no token or account; the server name and the player count can each be hidden.

### Changed
- Technical documentation in `docs/` is now in English; the Italian versions stay as `*.it.md`.
- Development: `src-tauri/.taurignore` keeps the `tauri dev` watcher from restarting the app whenever it writes its own settings files.

## [1.0.2] — 2026-09-02

Security hardening of the host listener (remote control and webhooks). Updating is recommended for anyone who has enabled the host or a webhook.

### Security
- Per-route request body limits. `/hook/{id}` now accepts at most 64 KB — it has to read the body to find the token, so an unauthenticated caller could previously make the host buffer up to 1 GB per request. API routes are capped at 1 MB and the server icon at 16 MB; the 1 GB limit remains only on mod uploads.
- Token comparison is constant-time (`subtle`), for the API token and for per-webhook tokens.
- Failed authentication is rate-limited per client address: after 20 failures within a minute the host answers `429` without evaluating the token.
- The API token is accepted only in the `Authorization: Bearer` header. The `?token=` query parameter is honoured solely on `/api/ws`, where browsers cannot set headers. Webhook tokens may still travel in query or body: they are per-hook, carry their own permissions, and GET-only integrations depend on it.
- CORS is restricted to the Tauri webview origins instead of `permissive()`. Non-browser clients (bots, the remote app) are unaffected.
- The listen address is configurable in **Settings → Remote control → Listen on**: whole network (default, `0.0.0.0`) or this PC only (`127.0.0.1`), for setups that go through a tunnel or a VPN on the same machine.
- Webhook call statistics no longer rewrite `settings.json` on every request. Unauthenticated calls are kept in memory only; authenticated ones are coalesced and flushed at most every 2 s and on exit. Every write to the settings file now goes through a single lock, so concurrent writers cannot clobber each other.
- A Content Security Policy is set for the webview (it was `null`): scripts and styles from the app only, fonts from Google Fonts, images from HTTPS CDNs, connections to local IPC and to remote hosts.

### Changed
- New application icon and logo, replacing the default Tauri artwork. The source is `site/assets/logo.svg`; every size under `src-tauri/icons/` is generated from it with `npm run tauri icon`.

## [1.0.1] — 2026-08-22

### Added
- English interface, in **American** and **British** spelling. All user-facing strings are localized, including backend error messages, progress text and console notices emitted by the app itself.
- Language selector in **Settings → Language**. The switch applies without restarting; the choice is stored in `settings.json` and reapplied at startup, backend messages included.
- Automatic language detection on first run: the OS locale is matched against the available languages (`it-IT` → Italian, `en-US` → English (US), `en-GB`/`en-AU` → English (UK)), falling back to English when the system language is not available. An explicit choice in Settings always takes precedence and is stored; leaving it unset keeps the app following the system.
- Translation completeness indicator next to each language, computed as the share of keys with a non-empty value against the source texts. Keys a translation does not cover fall back to English, then to the source text, instead of rendering blank.
- Translation workflow: `npm run lang:new -- <code>` generates a language file from the template, `npm run lang:template` regenerates the template after new strings are introduced. `cargo test --lib i18n` fails on missing keys or mismatched `{placeholders}`.

### Changed
- Settings dialog restructured: each area is a bordered card, disk usage has a fill bar, remote control shows an active/disabled state pill next to its heading, detected Java runtimes are listed with version and install count, and the footer states the ESC shortcut.
- Default language is now English rather than Italian: it applies on systems whose locale has no matching translation, and as the fallback for keys a translation has not covered yet.
- Strings moved out of the sources into `src/language/<code>.json`, read by both the frontend and the Rust backend (`include_str!`), so one file covers the whole application.

### Fixed
- Delete confirmation in English asked for `CONFERMA` while the button only unlocked on `CONFIRM`, making deletion impossible in that language. The required word now matches the active language.

## [1.0.0] — 2026-08-22

First public release.

### Server creation
- Three server kinds: **vanilla**, **plugins (Paper)** and **modded** (NeoForge, Forge, Fabric).
- Minecraft versions and loader builds pulled from the official sources (Mojang, PaperMC, Forge, NeoForge, Fabric), with the recommended build preselected and filterable lists.
- Import of an existing server from a ZIP archive, with automatic detection of version and launch method.

### Modpacks from a link
- CurseForge (official server packs, or a build from the client pack when no server pack is published), Modrinth (`.mrpack`) and FTB.
- Update detection with one-click install: world backup, user-data migration, rollback on failure.

### Mods and plugins
- Search and install from Modrinth and CurseForge, filtered by the server's Minecraft version and mod loader.
- Installed files record their origin (Modrinth / CurseForge / manual) and version.
- Per-mod updates, plus a bulk check from the toolbar.
- Enable and disable files without deleting them.

### Management
- Console with parsed logs, command input and live status.
- CPU/RAM metrics, disk space, uptime.
- `server.properties` editor, RAM setting, per-server UPnP toggle.
- World backups as zip archives.
- `server-icon.png` set from the app, resized to 64×64.
- Drag & drop server ordering and custom icons.
- Server deletion behind an explicit typed confirmation.
- Automatic Java runtime detection, including runtimes shipped with the Minecraft launcher.

### Network and integrations
- **Host mode**: servers managed remotely through an invite link, over a REST and WebSocket API.
- **Webhooks** per server with independent permissions (messages, commands with an allowlist, power, status) and a call log.
- UPnP port mapping at startup, with explicit handling of common router errors.

### Notes
- CurseForge requires a personal API key, entered in Settings: its terms do not permit distributing a shared key with the application. Modrinth and FTB need no key.
- The interface is available in Italian.
