# Changelog

Tutte le modifiche rilevanti del progetto. Il formato segue [Keep a Changelog](https://keepachangelog.com/it/1.1.0/); le versioni seguono [SemVer](https://semver.org/lang/it/).

## [1.0.0] — 2026-08-22

Prima versione pubblica.

### Creazione dei server
- Tre tipologie: **vanilla**, **plugin (Paper)** e **moddato** (NeoForge, Forge, Fabric).
- Versioni di Minecraft e build dei loader dalle fonti ufficiali (Mojang, PaperMC, Forge, NeoForge, Fabric), con la build consigliata preselezionata e liste filtrabili.
- Import di un server esistente da ZIP, con riconoscimento automatico di versione e modalità di avvio.

### Modpack da link
- CurseForge (server pack ufficiali, oppure costruzione dal pack client quando il server pack non esiste), Modrinth (`.mrpack`) e FTB.
- Notifica di aggiornamento e installazione in un clic: backup del mondo, migrazione dei dati utente, rollback in caso di errore.

### Mod e plugin
- Ricerca e installazione da Modrinth e CurseForge, filtrate per versione di Minecraft e mod loader del server.
- Ogni file mostra la propria origine (Modrinth / CurseForge / manuale) e la versione installata.
- Aggiornamento per singola mod, con controllo di massa dalla toolbar.
- Attivazione/disattivazione dei file senza cancellarli.

### Gestione
- Console con log colorati, invio comandi, stato in tempo reale.
- Metriche CPU/RAM, spazio su disco, uptime.
- Editor di `server.properties`, RAM, toggle UPnP per server.
- Backup del mondo in zip e ripristino dei file dal disco.
- `server-icon.png` impostabile dall'app con ridimensionamento automatico a 64×64.
- Riordino dei server con drag & drop e icone personalizzabili.
- Eliminazione di un server con conferma esplicita (bisogna scrivere `CONFERMA`).
- Rilevamento automatico delle installazioni Java, incluse quelle del launcher Minecraft.

### Rete e integrazioni
- Modalità **host**: i server si gestiscono da remoto tramite link d'invito, con API REST e WebSocket.
- **Webhook** per server con permessi separati (messaggi, comandi con lista consentita, accensione, stato) e registro delle chiamate.
- Apertura della porta via UPnP all'avvio, con messaggi chiari sugli errori tipici dei router.

### Note
- CurseForge richiede una **chiave API personale**, gratuita, da inserire in Impostazioni: le sue condizioni d'uso non consentono di distribuire una chiave condivisa nell'app. Modrinth e FTB funzionano senza chiave.
- L'interfaccia è in italiano.
