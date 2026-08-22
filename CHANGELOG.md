# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [1.0.1] — unreleased

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
