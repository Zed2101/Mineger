# Piano di implementazione — Mineger

Documento operativo: ogni fase ha N task con ID, file coinvolti e criterio di "fatto".
Le caselle vengono aggiornate man mano che i task vengono completati.

## Principi

- **Non rompere i server esistenti**: `server-data.json` resta compatibile (nuovi campi sempre `#[serde(default)]`).
- **Il backend è la fonte di verità**: lo stato di un server (starting/online/stopping/offline) è deciso in Rust ed emesso via eventi; il frontend non indovina con timer.
- **Nessuna azione irreversibile senza conferma** (EULA, cancellazione mod).
- **Verifica per fase**: `cargo check` (0 warning), `node --check` sui moduli JS, smoke test del frontend con mock di `window.__TAURI__` nel browser.

## Eventi Tauri (contratto backend → frontend)

| Evento | Payload | Quando |
|---|---|---|
| `server-output` | `{ id, line }` | ogni riga stdout/stderr del processo |
| `server-status` | `{ id, status: "starting"\|"online"\|"stopping"\|"offline", code?: number }` | ad ogni transizione di stato |

---

## Fase 1 — Ciclo di vita del processo (backend) ✅

| ID | Task | File | Fatto |
|---|---|---|---|
| 1.1 | `RunningServer { child, port, status }` al posto di `Child` nudo; thread monitor per server con `try_wait()` ogni 500 ms → rimozione dalla mappa + evento `server-status offline` con exit code | `utils.rs`, `commands.rs` | [x] |
| 1.2 | Evento `server-status`: `starting` dopo lo spawn, `online` quando il thread stdout vede `Done (…)! For help`, `stopping` dopo il comando stop, `offline` all'uscita | `commands.rs`, `utils.rs` | [x] |
| 1.3 | Riordino `start_server`: controlli (dir, data, jar, java) → spawn → UPnP in thread separato con esito scritto in console (`[Mineger] UPnP: …`); errore (non `Ok`) se già in esecuzione | `commands.rs` | [x] |
| 1.4 | `stop_server` invia `stop` e marca `stopping` senza rimuovere dalla mappa; nuovo comando `kill_server`; cleanup UPnP fatto dal monitor quando il processo esce davvero | `commands.rs`, `utils.rs`, `lib.rs` | [x] |
| 1.5 | Shutdown ordinato alla chiusura dell'app (`RunEvent::Exit`): `stop` a tutti, attesa max 10 s, kill dei residui, cleanup UPnP con una sola ricerca gateway | `lib.rs`, `utils.rs` | [x] |
| 1.6 | `get_servers` riporta lo stato reale (`starting/online/stopping/offline`) | `commands.rs` | [x] |

**Fatto quando**: un server che crasha torna `offline` da solo; chiudendo l'app non restano processi `java.exe`; `cargo check` pulito.

---

## Fase 2 — Frontend: stato coerente ✅

| ID | Task | File | Fatto |
|---|---|---|---|
| 2.1 | Fix bottone "Indietro" del wizard (non deve chiudere la modale) | `ui-modals.js` | [x] |
| 2.2 | Stato runtime centralizzato `state.runtime: Map<id, {status, logs}>`; listener `server-status`; rimozione del timer finto di 3 s | `main.js`, nuovo `ui-status.js` | [x] |
| 2.3 | Banner, bottone Avvia/Stop e pallino derivati solo dallo stato runtime; bottone "Forza arresto" visibile in `stopping` | `main.js`, `index.html`, `styles.css` | [x] |
| 2.4 | Buffer log per server (max 500 righe); cambiando server la console mostra il buffer; niente righe perse; fix `serverId` null | `ui-console.js` | [x] |
| 2.5 | `escapeHtml` per nomi server/mod; classe `.active` sul server selezionato; porta reale nella riga IP | `utils.js`, `main.js`, `ui-mods.js` | [x] |
| 2.6 | Bottone "Aggiorna" nella sidebar che richiama `get_servers` preservando selezione e stato runtime | `index.html`, `main.js` | [x] |

**Fatto quando**: avvio → cambio server → ritorno mostra stato e log corretti; stop di un server moddato resta "Spegnimento…" finché non esce davvero.

---

## Fase 3 — Percorsi, Java, EULA ✅

| ID | Task | File | Fatto |
|---|---|---|---|
| 3.1 | Modulo `paths.rs`: `servers_dir(app)` e `config_path(app)`; in debug usa `../servers` e `src/data/config.json` (setup attuale), in release `app_data_dir()/servers` e `app_config_dir()/config.json` | nuovo `paths.rs`, `commands.rs`, `utils.rs` | [x] |
| 3.2 | Modulo `java.rs`: auto-detect installazioni (JAVA_HOME, PATH, cartelle comuni di Windows/Linux/macOS), `java -version` → major; scelta = override in `config.json` → altrimenti la minima major installata ≥ richiesta; comando `get_java_runtimes` | nuovo `java.rs` | [x] |
| 3.3 | EULA: niente auto-accept. `start_server` ritorna errore `EULA_REQUIRED`; il frontend chiede conferma con link alla EULA; comando `accept_eula` | `commands.rs`, `main.js`, `index.html` | [x] |
| 3.4 | Registrazione `tauri-plugin-opener` + bottone "Apri cartella" nel tab Dettagli | `lib.rs`, `commands.rs`, `index.html`, `main.js` | [x] |

**Fatto quando**: su una macchina senza `config.json` l'app trova Java da sola; l'avvio di un server senza EULA chiede conferma.

---

## Fase 4 — Avvio flessibile e persistenza ✅

| ID | Task | File | Fatto |
|---|---|---|---|
| 4.1 | `LaunchConfig` in `server-data.json` (`max_ram_mb`, `jar`, `args_file`, `extra_jvm_args`, tutti opzionali). Risoluzione: `args_file` esplicito → `jar` esplicito → `server.jar` → auto-detect `libraries/net/{minecraftforge/forge,neoforged/neoforge}/*/win_args.txt` (`unix_args.txt` fuori da Windows). Include `@user_jvm_args.txt` se ha righe utili | `models.rs`, nuovo `launch.rs`, `commands.rs` | [x] |
| 4.2 | `launch_info` e `java_info` nell'entry server, mostrati nel tab Dettagli (diagnostica: "server.jar", "Forge args: …", "non avviabile: …") | `models.rs`, `commands.rs`, `index.html`, `main.js` | [x] |
| 4.3 | Comando `update_server_info(id, name, icon)` → scrive `server-data.json`; la modale Modifica lo usa | `commands.rs`, `ui-modals.js` | [x] |
| 4.4 | Comando `update_launch_config(id, max_ram_mb)`; campo RAM nel tab Proprietà popolato dal valore salvato | `commands.rs`, `ui-properties.js` | [x] |

**Fatto quando**: "Cave Horror" (Forge 1.20.1 senza server.jar) si avvia dall'app; rinominare un server sopravvive al riavvio.

---

## Fase 5 — Proprietà e Mod ✅

| ID | Task | File | Fatto |
|---|---|---|---|
| 5.1 | Comando `save_server_properties(id, props)` che riscrive solo i valori cambiati preservando commenti e ordine; bottone "Salva Modifiche" funzionante (salva anche la RAM) | `utils.rs`, `commands.rs`, `ui-properties.js` | [x] |
| 5.2 | Scan mod include `*.jar.disabled` con campo `enabled: false`; comando `toggle_mod(id, filename, enabled)` che rinomina; switch nella UI funzionante | `models.rs`, `utils.rs`, `commands.rs`, `ui-mods.js` | [x] |
| 5.3 | Comando `delete_mod(id, filename)` con conferma nella UI | `commands.rs`, `ui-mods.js` | [x] |
| 5.4 | Comando `add_mods(id)`: dialog nativo multi-file (`tauri-plugin-dialog`, lato Rust) + copia in `mods/`; bottone "Aggiungi Mod" funzionante | `Cargo.toml`, `capabilities/default.json`, `lib.rs`, `commands.rs`, `ui-mods.js` | [x] |

**Fatto quando**: cambiare MOTD/porta e premere Salva modifica `server.properties`; disattivare una mod crea `.jar.disabled`.

---

## Fase 6 — Creazione e import server ✅

| ID | Task | File | Fatto |
|---|---|---|---|
| 6.1 | Comando `get_vanilla_versions()` dal manifest Mojang (`piston-meta`), cache in memoria; il select del wizard si popola dinamicamente (release, ultime 30) | nuovo `create.rs`, `ui-modals.js` | [x] |
| 6.2 | Comando `create_vanilla_server(name, version)`: crea cartella, scarica `server.jar` con verifica SHA1 ed eventi `create-progress`, scrive `server-data.json` + `server.properties` dal template; la EULA viene chiesta al primo avvio | `create.rs`, `ui-modals.js`, `index.html` | [x] |
| 6.3 | Comando `import_server_zip(name)`: dialog nativo per lo zip, estrazione (gestisce lo zip con cartella radice singola), rilevamento versione (`version.json` nel bundler vanilla / nome `args_file` Forge-NeoForge), scrittura `server-data.json` | `Cargo.toml` (`zip`), `create.rs`, `ui-modals.js` | [x] |
| 6.4 | "Connetti remoto": card disabilitata con etichetta "Prossimamente" (fuori scope: richiede un protocollo di rete dedicato) | `index.html`, `ui-modals.js` | [x] |

**Fatto quando**: dal wizard si crea un server vanilla 1.21.x funzionante e si importa uno zip Forge.

---

## Fuori scope (futuro)

- Gestione server remoti (protocollo client/server Mineger).
- Schermata impostazioni per i percorsi Java (ora: auto-detect + override manuale in `config.json`).
- Backup/ripristino mondi, scheduler riavvii, statistiche CPU/RAM, whitelist/ops dalla UI.
- Build CI e installer firmati.

## Note tecniche

- Cartella server in **debug**: `<repo>/servers` (come ora). In **release**: `%APPDATA%/com.zed.mineger/servers`.
- Config Java in **debug**: `src-tauri/src/data/config.json` (override, machine-specific, da non committare in futuro). In **release**: `%APPDATA%/com.zed.mineger/config.json` (creato vuoto al primo avvio; l'auto-detect copre il caso normale).
- Le modifiche a proprietà/mod/RAM mentre il server gira sono permesse ma hanno effetto al riavvio (la UI lo segnala).

---

## Fase 7 — Redesign "Pannello Pro" (Tailwind) ✅

Riferimento: mockup 1440×900 + `DESIGN-NOTES.md` (palette, Archivo/Martian Mono, componenti).
CSS vanilla sostituito da **Tailwind v4** compilato con la CLI (`src/app.css` → `src/tailwind.css`, generato in build, gitignored).

| ID | Task | File | Fatto |
|---|---|---|---|
| 7.1 | Toolchain: `tailwindcss` + `@tailwindcss/cli`, script `css:build`/`css:watch`, `beforeDevCommand`/`beforeBuildCommand`, finestra 1440×900, font Google (Archivo, Martian Mono), `@theme` con la palette delle note e componenti (`btn-*`, `card`, `toggle`, `input`, `pill`, `tab`) | `package.json`, `tauri.conf.json`, `.gitignore`, `src/app.css` | [x] |
| 7.2 | Sidebar 276px: ricerca, riga "SERVER · n / n ATTIVO", item con ▶ avvio rapido se offline, footer (spazio su disco, Impostazioni, versione), "+ Aggiungi Server". Topbar: icona, nome + pill stato, `ip · uptime`, azioni (Avvia/Arresta, Forza arresto, Apri cartella, modifica) | `index.html`, `main.js`, `ui-status.js` | [x] |
| 7.3 | Tab Dettagli: tile CPU / RAM / TICK / UPTIME, grafico CPU-RAM a barre (campioni reali), Giocatori online (parsing `joined/left the game` dalla console), Backup mondo (lista + "Crea backup"), riga config VERSIONE / IP / JAVA / AVVIO | nuovo `ui-details.js`, `index.html` | [x] |
| 7.4 | Tab Mods: toolbar (titolo + GB totali, ricerca, filtri Tutte/Attive/Disattivate, + Aggiungi Mod), griglia 2 colonne, nota "effetto al riavvio" se online, "Apri cartella mods" | `ui-mods.js`, `index.html` | [x] |
| 7.5 | Tab Proprietà: 4 card 2×2 (Generale, Network, Gameplay & World, Server Launch Options con "Java rilevato"), barra fissa in basso con nota + Annulla + Salva | `ui-properties.js`, `index.html` | [x] |
| 7.6 | Tab Console: header (dot stato, titolo, n righe, AUTO-SCROLL, Pulisci), parser log `[hh:mm:ss] [tag]: msg` (vanilla e Forge) con colori timestamp/tag/INFO/eventi/WARN, chip "COMANDI RAPIDI", input con `>` e hint INVIO | `ui-console.js`, `index.html` | [x] |
| 7.7 | Backend: `started_at` per uptime (in `get_servers` e nell'evento `server-status`), `get_server_metrics` (CPU % e RAM del processo via `sysinfo`), `list_backups`/`create_backup` (zip del mondo in `backups/`, con `save-off/save-all/save-on` se online), `get_app_info` (versione, cartella server, spazio disco), `open_server_folder(id, sub)` | `process.rs`, nuovo `metrics.rs`, nuovo `backup.rs`, `commands.rs`, `models.rs`, `Cargo.toml` | [x] |
| 7.8 | Modali nel nuovo stile: Modifica, Nuovo server (wizard), EULA, Impostazioni (cartelle, Java rilevate con riscansione, versione) | `ui-modals.js`, `index.html` | [x] |
| 7.9 | Verifica: build CSS senza errori, smoke test nel browser con mock, `cargo test`, avvio `tauri dev` | — | [x] |

**Fatto quando**: le 4 schermate corrispondono ai mockup; nessun dato finto (TPS → indicatore lag reale, ping → orario di ingresso).

---

## Fase 8 — Controllo remoto ("Connetti Remoto") ✅

Un Mineger sul PC sempre acceso fa da **host** ed espone i suoi server via HTTP+WebSocket con token;
gli altri Mineger si collegano con un **link d'invito** (`mineger://IP:porta/#token`) e gestiscono quei server
come se fossero locali. Niente TLS in questa fase: pensato per LAN / VPN (Tailscale, ZeroTier) o port-forward consapevole.

| ID | Task | File | Fatto |
|---|---|---|---|
| 8.1 | UPnP allineato al test `upnp_test`: timeout 6 s, fallback lease (0 → 7 giorni), IP pubblico nel messaggio, hint troubleshooting (UPnP sul router, rete Privata) | `upnp.rs` | [x] |
| 8.2 | `service.rs`: logica dei comandi estratta da `commands.rs` (usabile sia dai comandi Tauri che dagli handler HTTP); ring buffer delle ultime 500 righe di console per server in `process.rs` + `get_recent_logs` (serve al client remoto che si collega a server già avviato) | nuovo `service.rs`, `process.rs`, `commands.rs` | [x] |
| 8.3 | `settings.rs`: `settings.json` nella cartella config (host: enabled/port/name/token; remote_hosts: id/name/url/token); comandi `get_settings`, `set_host_config`, `regenerate_host_token`, `add_remote_host` (verifica `/api/info` dal backend), `remove_remote_host` | nuovo `settings.rs`, `paths.rs` | [x] |
| 8.4 | `host.rs` + `events.rs`: server axum avviabile/arrestabile a caldo, bearer token, CORS, REST (`/api/info`, `/api/servers`, start/stop/kill/command/eula/metrics/backups/info/launch/properties/mods, upload mod multipart, `/logs`), WebSocket `/api/ws` che inoltra `server-output`/`server-status`/`backup-progress`; UPnP sulla porta di gestione; chiusura con l'app | nuovo `host.rs`, nuovo `events.rs`, `Cargo.toml`, `lib.rs` | [x] |
| 8.5 | Client: `remote.js` (`RemoteHost`: fetch con token, WebSocket con riconnessione, eventi → stesso `state.runtime`), `api.js` instrada i comandi per id `remote:<host>:<server>`, sidebar raggruppata per host con stato connessione, console seed da `/logs`, "Aggiungi mod" via `<input type=file>` + upload, azioni non disponibili da remoto nascoste (Apri cartella) | nuovo `remote.js`, `api.js`, `main.js`, `ui-status.js`, `ui-mods.js` | [x] |
| 8.6 | UI: wizard "Connetti Remoto" (link d'invito oppure IP:porta + token, test connessione); Impostazioni → sezione "Controllo remoto" (abilita, porta, nome, link d'invito con IP locale/pubblico e bottone Copia, rigenera token, stato UPnP); elenco host salvati con rimozione | `index.html`, `ui-modals.js` | [x] |
| 8.7 | Verifica: test Rust (parsing link, token, auth), e2e con l'app reale: host abilitato, `curl` su `/api/info` e `/api/servers`, WebSocket da Node, client che si collega a `127.0.0.1` | — | [x] |

**Fuori scope (8c, dopo)**: TLS/impronta nel link, ruoli amico/admin, relay cloud senza port-forward, creazione/import server da remoto, agent headless.

---

## Fase 9 — Usabilità: ordinamento, icone, webhook, "Aggiungi da link" ✅

| ID | Task | File | Fatto |
|---|---|---|---|
| 9.1 | Riordino server in sidebar con drag & drop (pointer events, ghost che segue il mouse, placeholder, animazioni FLIP, auto-scroll, click non compromesso); ordine persistito in `settings.server_order` (`set_server_order`), valido anche dentro i gruppi remoti | nuovo `sortable.js`, `main.js`, `settings.rs`, `commands.rs`, `app.css` | [x] |
| 9.2 | Icone server: cartella utente `icons/` (`paths::icons_dir`), comandi `list_icons` (builtin + utente, con data URL), `import_icon_from_path`, `pick_icon_file`, `delete_icon`; modale Modifica con griglia di anteprime selezionabili, drop di un file (evento `tauri://drag-drop`) o "Sfoglia…"; `iconUrl()` con cache nel frontend | nuovo `icons.rs`, `paths.rs`, `commands.rs`, `ui-modals.js`, `main.js`, `index.html` | [x] |
| 9.3 | Webhook in ingresso: `settings.webhooks` (id, nome, token, server, permessi say/command/power/status, allowlist comandi, abilitato); route `POST/GET /hook/{id}` (JSON, form o query; token via Bearer/`token`), `say` → `tellraw`, `command` con allowlist (no `stop`), `start`/`stop`, `status`; audit in console; listener attivo se host o webhook abilitati; comandi CRUD; UI Impostazioni (crea, abilita, elimina, copia URL/token, esempio curl) | `settings.rs`, `host.rs`, `commands.rs`, `ui-modals.js`, `index.html`, `Cargo.toml` | [x] |
| 9.4 | Wizard: card "Aggiungi da link" con campo URL (placeholder in attesa di specifiche, conferma disabilitata) | `index.html`, `ui-modals.js` | [x] |
| 9.5 | Verifica: test Rust (allowlist, merge richiesta webhook, sanificazione nomi icona), mock UI (DnD simulato con pointer events, icone, webhook), e2e su app reale (`curl` su `/hook/...`: 401/403/400, `status`, `start` + `say` + `stop` su un server vero) | — | [x] |
| 9.6 | Webhook spostati dalle Impostazioni a un **tab "Webhook" per server**: card con permessi, contatore chiamate, ultima chiamata (quando/azione/esito/IP), endpoint, token mascherato con Mostra/Copia, esempio curl, **Prova** (chiamata reale all'endpoint locale), Elimina; pannello "Ultime chiamate" live (evento `webhook-call`); statistiche persistite in `settings.json`; IP chiamante via `ConnectInfo` | `host.rs`, `commands.rs`, nuovo `ui-webhooks.js`, `index.html`, `ui-modals.js` | [x] |
| 9.7 | Toggle **UPnP per server** in Proprietà → Server Launch Options (`launch.upnp`, default attivo), rispettato all'avvio; anche via API remota (`PUT /launch`) | `models.rs`, `process.rs`, `service.rs`, `ui-properties.js`, `main.js`, `remote.js` | [x] |
| 9.8 | **Icona server per la lista multiplayer** (`server-icon.png`) in Proprietà: anteprima, "Scegli immagine…" (dialog locale / file input remoto), "Usa l'icona di Mineger", drop di un file, Rimuovi; il backend (`servericon.rs`, crate `image`) ridimensiona qualsiasi PNG/JPG/WEBP/GIF/BMP a **64×64** con ritaglio centrato e salva in PNG; API `GET/PUT/DELETE /api/servers/{id}/server-icon` | nuovo `servericon.rs`, `service.rs`, `commands.rs`, `host.rs`, `ui-properties.js`, `remote.js`, `index.html` | [x] |

---

## Fase 10/11 — Server da link: CurseForge, Modrinth, FTB + aggiornamenti ✅

| ID | Task | File | Fatto |
|---|---|---|---|
| 10.1 | `providers/`: parsing link e metadati per **CurseForge** (API v1 con chiave, server pack via `isServerPack`/`serverPackFileId`, sha1, `download-url`, rispetto di `allowModDistribution`), **Modrinth** (API v2, versioni `.mrpack`), **FTB** (api.modpacks.ch, versioni + file + targets) | nuovo `providers/{mod,curseforge,modrinth,ftb}.rs` | [x] |
| 10.2 | `packs.rs`: download con progress e verifica SHA1; installazione da server pack zip (riuso import), da `.mrpack` (indice, file `env.server`, overrides) e da FTB (file non client-only); installazione loader **Fabric** (meta API), **Forge**/**NeoForge** (installer `--installServer`); `source` salvata in `server-data.json` | nuovo `packs.rs`, `models.rs`, `create.rs` | [x] |
| 10.3 | Aggiornamenti: `check_updates` (all'avvio + ogni 6 h + manuale), cache, evento `pack-updates`; `update_pack_server`: stop, backup mondo, installazione in cartella temporanea, migrazione dati utente (mondo, properties, eula, ops/whitelist/banned, usercache, icona, backup, server-data), mod extra in `mods-precedenti/`, scambio con rollback `.old-…` | `packs.rs`, `commands.rs`, `service.rs` | [x] |
| 10.4 | UI: wizard "Aggiungi da link" (cerca → scheda pack → scelta versione → installa con progress); card "Modpack" in Dettagli (sorgente, versione, Controlla/Aggiorna con progress, pagina); badge aggiornamento in sidebar; chiave CurseForge in Impostazioni | `index.html`, nuovo `ui-packs.js`, `ui-modals.js`, `main.js`, `settings.rs` | [x] |
| 10.5 | Verifica: test Rust (parsing link, loader da gameVersions, mrpack index, FTB targets, migrazione) ✔; e2e reali: Modrinth (risoluzione + **installazione reale** di Harpy Express in 22 s + controllo aggiornamenti) ✔, FTB (risoluzione FTB Skies) ✔, CurseForge ✔ (ATM10: 140 server pack, 8649107 suggerito, URL CDN e SHA1 presenti) | — | [x] |
| 10.6 | **Pack CurseForge senza server pack** (es. FTB StoneBlock 4): voce "Build dal client" — lista file con URL diretti, SHA1 e flag client-only dall'API FTB (`/public/curseforge/{id}`), mod solo-client escluse, loader (NeoForge/Forge/Fabric) installato automaticamente; "stub" installer < 1 MB esclusi dai server pack; aggiornamenti filtrati per tipo (`SourceInfo.kind`). Test live installer NeoForge 21.1.248 ✔, e2e StoneBlock 4 (risoluzione 43 build + installazione reale 642 MB) | — | [x] |
| 10.7 | Wizard "Aggiungi da link": lista versioni ridisegnata al posto del dropdown (righe radio con badge consigliata/tipo, MC/loader, data/dimensione, "mostra tutte", frecce da tastiera); modale scrollabile entro il viewport | — | [x] |
| 10.8 | **Bug console**: Java su Windows scrive stdout nella codepage di sistema; un carattere non ASCII (es. `©` nel banner di Konkrete) interrompeva il lettore (`lines()` → errore UTF-8 → thread terminato): console muta e stato bloccato su "starting". Fix: lettura byte-per-riga con decodifica lossy (`for_each_line_lossy`) + flag JVM `-Dstdout.encoding/-Dstderr.encoding` (e `sun.*` per Java < 19). Verificato con SB4 (381 mod): "Done" rilevato → online | — | [x] |

## Fase 12 — Elimina server ✅

| # | Task | Note | Stato |
|---|------|------|-------|
| 12.1 | Backend `service::delete_server` (rifiuta se in esecuzione, controllo `is_inside` sotto servers/, retry su file bloccati Windows, pulizia server_order/cache aggiornamenti/buffer console) + `server_disk_usage`; comandi Tauri e route host `DELETE /api/servers/{id}` e `/disk-usage`; instradamento remoto | — | [x] |
| 12.2 | UI: bottone "Elimina server…" nella modale Modifica → modale dedicata con dimensione/conteggio file, avviso irreversibile, conferma scrivendo **CONFERMA** (bottone attivo solo con match esatto), guardia se in esecuzione | — | [x] |
| 12.3 | Verifica: unit test (`dir_usage`, `is_inside`), mock UI (parola esatta, sparizione, empty state), e2e reale (disk-usage, rifiuto path traversal, 401 senza token, eliminazione + rimozione da server_order, doppia delete) | — | [x] |
| 12.4 | Rimosso il badge "Presto" dalla card "Aggiungi da link" nel wizard | — | [x] |

## Fase 13 — Creazione guidata (vanilla / plugin / moddato) e catalogo mod ✅

| # | Task | Note | Stato |
|---|------|------|-------|
| 13.1 | `loaders.rs`: tipi di server (vanilla, Paper, Forge, NeoForge, Fabric) e liste versioni dalle **fonti ufficiali** — Mojang (manifest), PaperMC (`fill.papermc.io/v3`), Forge (maven-metadata + promotions_slim), NeoForge (maven API), Fabric (meta). Build "consigliata" evidenziata; jar Paper verificato con SHA256 | — | [x] |
| 13.2 | Creazione server per tipo: Paper scarica il server jar e prepara `plugins/`; Forge/NeoForge/Fabric installano il loader ufficiale e preparano `mods/`; il piano di avvio viene validato subito dopo la creazione | — | [x] |
| 13.3 | Wizard ridisegnato: 3 tipologie (Vanilla / Plugin / Moddato + scelta loader) e **liste custom** (niente dropdown) per versione MC e build, con filtro, badge consigliata/beta/latest e data | — | [x] |
| 13.4 | `providers/mods.rs`: ricerca e versioni di **singole mod/plugin** su Modrinth (`/v2/search`) e CurseForge (classId 6 mod, 5 plugin), filtrate per versione MC e loader (i plugin accettano la famiglia paper/spigot/bukkit/purpur). FTB non ha catalogo di singole mod: resta solo per i modpack | — | [x] |
| 13.5 | `modsvc.rs`: installazione nella cartella giusta (`mods/` o `plugins/`), **registro delle sorgenti** in `server-data.json`, controllo aggiornamenti per mod e aggiornamento con sostituzione del jar (mantiene attiva/disattivata, pulisce il vecchio file) | — | [x] |
| 13.6 | Tab Mods: browser di installazione con selettore fonte, ricerca, versioni compatibili e progress; **badge della fonte** (Modrinth / CurseForge / manuale), versione installata, pulsante **aggiorna → x.y.z** per mod, "Aggiornamenti" per il controllo di massa. Le mod manuali non sono aggiornabili (per scelta: non c'è modo di sapere da dove vengano) | — | [x] |
| 13.7 | Route host per client remoti: creazione server, liste versioni, ricerca/versioni/installazione/aggiornamento mod | — | [x] |
| 13.8 | Verifica: unit test Rust (tipi, prefisso NeoForge, ordinamento versioni, famiglie loader, parsing versioni, registro) ✔; test live liste ufficiali ✔; mock UI (wizard 3 tipi, filtri, badge, aggiornamento, caso vanilla, errore chiave API) ✔; e2e reale (creazione Paper + Fabric, installazione mod da Modrinth e CurseForge, aggiornamenti) ✔ | — | [x] |
| 13.9 | Difetti emersi dagli e2e reali e corretti: (a) CurseForge usa spesso la versione di Minecraft come nome del file → la lista mostrava "v1.21.1" invece di "v0.8.13": ora la versione MC viene scartata a favore di quella estratta dal nome file; (b) su Modrinth i plugin hanno `project_type:plugin` (con `mod` la ricerca torna **sempre vuota**); (c) fallback senza filtro versione quando una release MC non è ancora indicizzata; (d) niente lista versioni quando l'autore vieta il download da app terze | — | [x] |
| 13.10 | Ricerca a vuoto su versioni MC appena uscite (caso reale: server 26.2, "Applied Energistics 2" → 0 risultati mentre su 1.21.1 ce ne sono 50): la ricerca ripete **senza il filtro versione ma mantenendo sempre quello del loader**, avvisa in chiaro ("nessuna mod per Minecraft X: mostro N progetti <loader> per altre versioni") e marca le build con badge **altra versione**. Il filtro loader non viene mai allentato, né in ricerca né nella lista versioni | — | [x] |
