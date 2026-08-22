# Mineger

Gestore di server Minecraft per Windows: crea, avvia e amministra i tuoi server da un'unica app, senza righe di comando.

Mineger è un'applicazione desktop costruita con [Tauri](https://tauri.app) — backend in Rust, interfaccia in HTML/JS con Tailwind CSS. Nessun account, nessun servizio esterno obbligatorio: i server restano cartelle sul tuo disco.

---

## Cosa sa fare

### Creare server
- **Vanilla** — server ufficiale Mojang, scaricato e verificato con SHA1.
- **Plugin (Paper)** — server ottimizzato compatibile con i plugin Bukkit/Spigot.
- **Moddato** — **NeoForge**, **Forge** o **Fabric**, installati con gli installer ufficiali.

Le versioni di Minecraft e le build dei loader arrivano dalle fonti ufficiali (Mojang, PaperMC, Forge, NeoForge, Fabric), con la build consigliata già selezionata.

### Installare da un link
Incolla il link di un modpack e Mineger prepara il server:
- **CurseForge** — server pack ufficiali; se il modpack non ne pubblica uno, il server viene costruito dal pack client (mod solo-client escluse).
- **Modrinth** — pacchetti `.mrpack`.
- **FTB** — modpack del launcher Feed The Beast.

Quando esce una nuova versione del modpack l'app te lo segnala e la installa con un clic, facendo prima il backup del mondo e conservando i tuoi dati (mondo, configurazioni, backup).

### Mod e plugin
- Ricerca e installazione diretta da **Modrinth** e **CurseForge**, filtrate per la versione di Minecraft e il mod loader del tuo server.
- Ogni file installato mostra **da dove arriva** (Modrinth, CurseForge o *manuale*) e la sua versione.
- Pulsante **aggiorna** sulle mod per cui è uscita una versione più recente.
- Attivazione/disattivazione senza cancellare i file (`.jar` ⇄ `.jar.disabled`).

### Gestione quotidiana
- Avvio e arresto con stato in tempo reale, console con i log colorati e invio comandi.
- Grafici CPU/RAM del processo Java, spazio su disco, uptime.
- Editor di `server.properties` con descrizioni dei campi.
- Backup del mondo in zip (con `save-off`/`save-all` per una copia consistente).
- Icona del server (`server-icon.png`) ridimensionata automaticamente a 64×64.
- Riordino dei server con drag & drop, icone personalizzabili.
- **Apertura porta via UPnP** all'avvio, attivabile per singolo server.
- Rilevamento automatico delle installazioni Java (incluse quelle del launcher Minecraft).

### Controllo remoto e integrazioni
- **Host**: un'installazione di Mineger può esporre i suoi server in rete; gli amici li avviano e gestiscono dalla loro copia dell'app tramite un link d'invito.
- **Webhook** per server: consentono a bot Discord, estensioni Twitch o qualsiasi servizio HTTP di inviare messaggi in chat, eseguire comandi da una lista consentita, avviare/fermare il server o leggerne lo stato — con permessi separati per ogni webhook.

---

## Installazione

1. Scarica l'installer dalla pagina [Releases](https://github.com/Zed2101/Mineger/releases).
2. Esegui `Mineger_1.0.0_x64-setup.exe` (oppure il `.msi`).
3. Avvia Mineger.

**Requisiti**
- Windows 10/11 a 64 bit.
- **Java** installato: 8/17/21 a seconda della versione di Minecraft (Mineger rileva le installazioni presenti e ti dice quale userà). Le versioni recenti richiedono Java 21.

---

## Chiave API di CurseForge

Modrinth e FTB funzionano subito. **CurseForge richiede una chiave API personale**: le sue condizioni d'uso non permettono di distribuire una chiave condivisa dentro l'applicazione, quindi ognuno usa la propria.

È gratuita:

1. Vai su [console.curseforge.com](https://console.curseforge.com/#/api-keys) e accedi.
2. Genera una API key.
3. In Mineger: **Impostazioni → Chiave CurseForge**, incolla e salva.

Senza chiave, mod e modpack di CurseForge non sono disponibili e l'app te lo dice dove serve; tutto il resto (Modrinth, FTB, vanilla, Paper, loader) funziona normalmente.

---

## Primo avvio

1. **+ Aggiungi Server** → scegli come crearlo:
   - **Crea nuovo** — tipo (vanilla / plugin / moddato), versione di Minecraft, build del loader.
   - **Importa ZIP** — un server che hai già.
   - **Aggiungi da link** — modpack CurseForge / Modrinth / FTB.
   - **Connetti remoto** — un server ospitato sul PC di un amico.
2. Al primo avvio Mineger chiede di accettare la [EULA di Minecraft](https://aka.ms/MinecraftEULA).
3. In **Proprietà** imposta la RAM (i modpack grossi ne vogliono 6 GB o più) e decidi se aprire la porta con UPnP.

I server stanno in `servers/<nome>` accanto all'app (in sviluppo, nella cartella del progetto).

---

## Giocare con gli amici

Tre strade, dalla più semplice:

1. **Stessa rete locale** — gli amici si collegano all'IP locale mostrato nella schermata Dettagli.
2. **UPnP** — se il router lo consente, Mineger apre la porta all'avvio e mostra l'IP pubblico. Se il router risponde con un errore di conflitto significa che la porta è già inoltrata a mano: in quel caso va già bene così.
3. **Host remoto** — chi ha il PC sempre acceso attiva l'host in **Impostazioni → Host** e condivide il link d'invito: gli altri lo incollano in *Connetti remoto* e gestiscono il server a distanza.

---

## Sviluppo

```bash
npm install
npm run tauri dev
```

- `npm run css:build` / `css:watch` — compila Tailwind (`src/app.css` → `src/tailwind.css`).
- `npm run tauri build` — genera gli installer in `src-tauri/target/release/bundle/`.
- `cd src-tauri && cargo test --lib` — suite di test del backend.
- I test marcati `#[ignore]` toccano la rete o installano loader veri: `cargo test --lib -- --ignored`.

Servono [Rust](https://rustup.rs) e Node.js 18+.

Documentazione tecnica: [architettura](docs/ARCHITETTURA.md) · [API host e webhook](docs/API-HOST.md).

---

## Licenza

MIT — vedi [LICENSE](LICENSE).

Mineger non è affiliato con Mojang, Microsoft, Overwolf/CurseForge, Modrinth o Feed The Beast. Minecraft è un marchio di Mojang AB.
