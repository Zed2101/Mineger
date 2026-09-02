# Architettura

*[Read this file in English](ARCHITECTURE.md)*

Mineger è un'app [Tauri v2](https://tauri.app): backend Rust, frontend HTML/CSS/JS senza framework né bundler (moduli ES nativi), Tailwind CSS v4 per lo stile.

```
src/                    frontend (servito da Tauri come frontendDist)
  index.html            tutta la UI: sidebar, tab, modali
  app.css → tailwind.css  sorgente Tailwind e output generato (gitignored)
  main.js               avvio, stato globale, routing tra le viste
  modules/              un modulo per area funzionale
src-tauri/
  src/                  backend Rust
  tauri.conf.json       finestra, bundle, permessi
```

## Principio di base

La logica sta in **`service.rs`** e nei moduli di dominio, indipendente dal trasporto. Sopra ci sono due sole facciate:

- **`commands.rs`** — comandi Tauri chiamati dal frontend locale (`invoke`).
- **`host.rs`** — server HTTP/WebSocket [axum](https://github.com/tokio-rs/axum) per i client remoti.

Entrambe chiamano le stesse funzioni: una nuova funzionalità si scrive una volta e vale per locale e remoto.

## Moduli backend

| Modulo | Responsabilità |
|---|---|
| `service.rs` | Elenco server, avvio/arresto, mod, proprietà, backup, icone, eliminazione |
| `process.rs` | Processi Java: spawn, stdin, lettura log (tollerante alle codifiche non UTF-8), rilevamento uscita, stato |
| `launch.rs` | Come avviare un server: `server.jar`, file argomenti Forge/NeoForge, `user_jvm_args.txt`, RAM |
| `java.rs` | Rilevamento delle installazioni Java e scelta della versione adatta alla release di Minecraft |
| `create.rs` | Creazione vanilla (manifest Mojang), import da ZIP, riconoscimento dei server importati |
| `loaders.rs` | Tipi di server e liste versioni ufficiali: Mojang, PaperMC, Forge, NeoForge, Fabric; creazione del server |
| `packs.rs` | Modpack: installazione, aggiornamenti periodici, migrazione dei dati utente, rollback |
| `providers/` | Client delle piattaforme: `curseforge`, `modrinth`, `ftb`, `mods` (singole mod/plugin) |
| `modsvc.rs` | Installazione mod/plugin, registro delle sorgenti, aggiornamenti per singola mod |
| `upnp.rs` | Apertura porta sul router (igd-next), gestione degli errori tipici dei router |
| `host.rs` | API REST + WebSocket, autenticazione a token, webhook pubblici |
| `settings.rs` | `settings.json`: host, host remoti, webhook, ordine dei server, chiave CurseForge |
| `events.rs` | Bus di eventi condiviso tra Tauri e WebSocket |
| `backup.rs` · `servericon.rs` · `icons.rs` · `metrics.rs` | Backup del mondo, `server-icon.png` 64×64, icone dei server, campionamento CPU/RAM |

## Moduli frontend

| Modulo | Responsabilità |
|---|---|
| `api.js` | Instrada le chiamate: locali via `invoke`, remote (`remote:<host>:<id>`) via `RemoteHost` |
| `remote.js` | Client REST + WebSocket con riconnessione automatica |
| `ui-status.js` · `ui-console.js` | Stato dei server, console con parser dei log |
| `ui-details.js` · `ui-properties.js` | Schermata Dettagli, editor di `server.properties`, UPnP, icona |
| `ui-mods.js` · `ui-modbrowser.js` | Elenco mod/plugin con fonte e aggiornamenti; ricerca e installazione dalle piattaforme |
| `ui-modals.js` | Modifica server, wizard di creazione, impostazioni, eliminazione con conferma |
| `ui-packs.js` | Scheda modpack, badge aggiornamenti |
| `ui-webhooks.js` | Tab Webhook per server |
| `sortable.js` · `icons.js` · `ui-tabs.js` · `utils.js` | Drag & drop, icone, tab, formattazione |

## Dati su disco

```
servers/<nome>/
  server-data.json      nome, icona, versione, tipo, opzioni di avvio,
                        sorgente del modpack, registro delle mod installate
  server.properties     configurazione del gioco
  mods/ | plugins/      contenuti caricabili (secondo il tipo di server)
  backups/              zip dei mondi
settings.json           impostazioni dell'app, nella cartella config
                        (src-tauri/src/data/ nelle build di sviluppo, non versionato)
```

`server-data.json` è la fonte di verità dell'app; lo stato di esecuzione vive solo in memoria (`process.rs`) e viene ricalcolato a ogni avvio.

## Eventi

Il backend emette eventi che la UI ascolta senza fare polling:

| Evento | Quando |
|---|---|
| `server-status` | Cambio di stato (starting/online/stopping/offline) |
| `server-output` | Nuova riga di console |
| `create-progress` · `update-progress` · `mod-progress` | Avanzamento di creazione, aggiornamento modpack, download mod |
| `pack-updates` | Esito del controllo aggiornamenti dei modpack |
| `backup-progress` · `webhook-call` | Backup in corso, webhook ricevuto |

Gli stessi eventi viaggiano sul WebSocket dell'host verso i client remoti.

## Test

`cargo test --lib` copre la logica pura: parsing dei link, riconoscimento dei loader, versioni, percorsi sicuri, registro delle mod, migrazione dei dati, decodifica delle risposte delle API (con esempi reali salvati).

I test `#[ignore]` fanno cose vere — scaricano liste ufficiali, installano NeoForge — e si lanciano a parte con `--ignored`.
