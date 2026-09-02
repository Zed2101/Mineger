# Mineger

A Minecraft server manager for Windows: create, run and administer your servers from a single app — no command line required.

Mineger is a desktop application built with [Tauri](https://tauri.app): a Rust backend and an HTML/JS interface styled with Tailwind CSS. No account, no mandatory external service — your servers stay as folders on your own disk.

*[Leggi questo file in italiano](README.it.md)* · **Website:** [zed2101.github.io/Mineger](https://zed2101.github.io/Mineger/)

---

## What it does

### Create servers
- **Vanilla** — the official Mojang server, downloaded and verified against its SHA1.
- **Plugins (Paper)** — the optimised server that runs Bukkit/Spigot plugins.
- **Modded** — **NeoForge**, **Forge** or **Fabric**, installed with the official installers.

Minecraft versions and loader builds come from the official sources (Mojang, PaperMC, Forge, NeoForge, Fabric), with the recommended build preselected.

### Install from a link
Paste a modpack link and Mineger sets the server up for you:
- **CurseForge** — official server packs; when a modpack doesn't publish one, the server is built from the client pack (client-only mods left out).
- **Modrinth** — `.mrpack` files.
- **FTB** — modpacks from the Feed The Beast launcher.

When a new version of the modpack is released the app tells you and installs it in one click, backing up the world first and keeping your data (world, configs, backups).

### Mods and plugins
- Search and install directly from **Modrinth** and **CurseForge**, filtered by your server's Minecraft version and mod loader.
- Every installed file shows **where it came from** (Modrinth, CurseForge or *manual*) along with its version.
- An **update** button appears on mods that have a newer build available.
- Enable and disable files without deleting them (`.jar` ⇄ `.jar.disabled`).

### Day-to-day management
- Start and stop with live status, a console with colour-coded logs, and command input.
- CPU/RAM usage of the Java process, disk space, uptime.
- A `server.properties` editor with plain-language descriptions.
- World backups as zip archives (using `save-off`/`save-all` for a consistent copy).
- Server icon (`server-icon.png`) resized to 64×64 automatically.
- Drag & drop reordering, customisable icons.
- **UPnP port mapping** on startup, switchable per server.
- Automatic detection of installed Java runtimes, including those shipped with the Minecraft launcher.

### Remote control and integrations
- **Host mode**: one Mineger installation can expose its servers over the network, so friends start and manage them from their own copy of the app through an invite link.
- **Webhooks** per server: let Discord bots, Twitch extensions or any HTTP service send chat messages, run commands from an allowlist, start/stop the server or read its status — each webhook with its own permissions.

---

## Installation

1. Download the installer from the [Releases](https://github.com/Zed2101/Mineger/releases) page.
2. Run `Mineger_1.0.1_x64-setup.exe` (or the `.msi`).
3. Launch Mineger.

**Requirements**
- 64-bit Windows 10/11.
- **Java** installed: 8/17/21 depending on the Minecraft version (Mineger detects what you have and tells you which runtime it will use). Recent versions need Java 21.

---

## CurseForge API key

Modrinth and FTB work out of the box. **CurseForge requires a personal API key**: its terms don't allow shipping a shared key inside an application, so everyone uses their own.

It's free:

1. Go to [console.curseforge.com](https://console.curseforge.com/#/api-keys) and sign in.
2. Generate an API key.
3. In Mineger: **Settings → CurseForge key**, paste it and save.

Without a key, CurseForge mods and modpacks are unavailable and the app says so wherever it matters; everything else (Modrinth, FTB, vanilla, Paper, loaders) works normally.

---

## Getting started

1. **+ Add server** → pick how to create it:
   - **Create new** — kind (vanilla / plugins / modded), Minecraft version, loader build.
   - **Import ZIP** — a server you already have.
   - **Add from link** — a CurseForge / Modrinth / FTB modpack.
   - **Connect remote** — a server hosted on a friend's PC.
2. On first start Mineger asks you to accept the [Minecraft EULA](https://aka.ms/MinecraftEULA).
3. Under **Properties**, set the RAM (large modpacks want 6 GB or more) and choose whether to open the port via UPnP.

Servers live in `servers/<name>` next to the app (in development, inside the project folder).

---

## Playing with friends

Three options, simplest first:

1. **Same local network** — friends connect to the local IP shown on the Details screen.
2. **UPnP** — if the router allows it, Mineger opens the port on startup and shows your public IP. A conflict error from the router usually means the port is already forwarded manually, which works just as well.
3. **Remote host** — whoever keeps a PC running enables the host under **Settings → Host** and shares the invite link; the others paste it into *Connect remote* and manage the server from afar.

---

## Development

```bash
npm install
npm run tauri dev
```

- `npm run css:build` / `css:watch` — compile Tailwind (`src/app.css` → `src/tailwind.css`).
- `npm run tauri build` — produce the installers under `src-tauri/target/release/bundle/`.
- `cd src-tauri && cargo test --lib` — backend test suite.
- Tests marked `#[ignore]` hit the network or install real loaders: `cargo test --lib -- --ignored`.

You'll need [Rust](https://rustup.rs) and Node.js 18+.

Technical documentation: [architecture](docs/ARCHITECTURE.md) · [host API and webhooks](docs/API-HOST.md).

---

## Translating Mineger

The interface ships in **English** — American or British spelling — and **Italian**. On first run Mineger follows your operating system’s language and falls back to English when the system language isn’t available. A US system gets English (US), other English locales get English (UK). You can override it in **Settings → Language** — the change is immediate and the choice is remembered from then on. Next to each language you’ll see how complete its translation is.

Every string in the app lives in `src/language/<code>.json`. That one file is read by both the interface and the Rust backend, so translating it covers the whole app — screens, buttons and error messages alike.

### Adding a language

**1. Create the file from the template**

```bash
npm run lang:new -- fr
```

This writes `src/language/fr.json` containing every key with an empty value, and prints the remaining steps. Use the [ISO 639-1 code](https://en.wikipedia.org/wiki/List_of_ISO_639_language_codes) (`fr`, `de`, `es`, `pt-BR`…).

**2. Translate the values, not the keys**

```jsonc
{
  "ui": {
    "topbar": {
      "start": "Démarrer le serveur"   // ← translate this
    }
  },
  "errors": {
    "folder_exists": "Un serveur existe déjà dans le dossier « {id} »"
  }
}
```

Two rules:
- **Keys never change** — they're how the app finds the text.
- **Placeholders like `{id}`, `{count}`, `{name}` must stay**, spelled exactly the same. They're replaced at runtime with real values; the surrounding words can move freely to fit your grammar.
- Some entries have `one` / `other` sub-keys for singular and plural — fill in both.
- Product names (Minecraft, CurseForge, Modrinth, FTB, Paper, NeoForge, Forge, Fabric, Java, UPnP) stay as they are.

You don't have to finish in one go: untranslated keys fall back to Italian, and Settings shows the completion percentage.

**3. Register the language in two places**

`src/modules/i18n.js` — add an entry to `LANGUAGES`:

```js
export const LANGUAGES = [
  { code: 'it', name: 'Italiano', english: 'Italian', flag: '🇮🇹' },
  { code: 'en', name: 'English',  english: 'English', flag: '🇬🇧' },
  { code: 'fr', name: 'Français', english: 'French',  flag: '🇫🇷' },   // ← new
];
```

`src-tauri/src/i18n.rs` — add the constant and the entry in `available()`:

```rust
const FR: &str = include_str!("../../src/language/fr.json");   // ← new

pub fn available() -> Vec<(&'static str, &'static str)> {
    vec![("it", "Italiano"), ("en", "English"), ("fr", "Français")]  // ← new
}
```

…and add it to the dictionary cache in the same file, next to the existing `m.insert("en", flatten(EN));`:

```rust
m.insert("fr", flatten(FR));
```

**4. Check your work**

```bash
cd src-tauri && cargo test --lib i18n
```

These tests fail if a language is missing keys or if a `{placeholder}` doesn't match the Italian original — so a half-finished translation shows up as a test failure instead of stray text in the app. Then run `npm run tauri dev`, open **Settings → Language** and pick your language: everything should switch instantly.

### When the app gains new strings

Run `npm run lang:template` to refresh `src/language/_template.json`, then add the new keys to each language file. `cargo test --lib i18n` lists exactly which keys are missing.

---

## Licence

MIT — see [LICENSE](LICENSE).

Mineger is not affiliated with Mojang, Microsoft, Overwolf/CurseForge, Modrinth or Feed The Beast. Minecraft is a trademark of Mojang AB.
