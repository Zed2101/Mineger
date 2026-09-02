# API host e webhook

Un'installazione di Mineger può fare da **host**: espone i suoi server in rete, così altre copie dell'app (o qualsiasi client HTTP) li gestiscono a distanza. Si attiva in **Impostazioni → Host**.

L'host serve due cose diverse:

- **API di controllo** sotto `/api/...`, protetta da token — è quella che usa l'app remota.
- **Webhook** sotto `/hook/<id>`, pubblici ma con token e permessi propri — pensati per bot Discord, estensioni Twitch, script.

Porta predefinita: **25580**.

---

## Autenticazione

Ogni richiesta a `/api/...` richiede il token dell'host:

```
Authorization: Bearer <token>
```

Il link d'invito che l'app genera (`mineger://…`) contiene indirizzo, porta e token: incollandolo in *Connetti remoto* il client si configura da solo.

> Il token dà **controllo completo** sui server dell'host: condividilo solo con chi vuoi che li amministri.

Il token è accettato **solo nell'header**. L'unica eccezione è `/api/ws?token=…`, perché un browser non può impostare header su un WebSocket: lì il token viaggia nella query string.

### Limiti e protezioni

| Cosa | Valore |
|---|---|
| Body massimo `/api/...` | 1 MB (16 MB per l'icona del server, 1 GB solo per l'upload delle mod) |
| Body massimo `/hook/{id}` | 64 KB |
| Tentativi di autenticazione falliti | 20 al minuto per indirizzo, poi `429 Too Many Requests` |
| Confronto del token | a tempo costante |
| CORS | solo le origini del webview Mineger (i client non-browser non ne sono toccati) |
| Indirizzo di ascolto | **Impostazioni → Controllo remoto → In ascolto su**: tutta la rete (predefinito) o solo questo PC |

---

## Endpoint

### Generali

| Metodo | Percorso | Descrizione |
|---|---|---|
| `GET` | `/api/info` | Nome dell'host, versione, spazio su disco |
| `GET` | `/api/servers` | Elenco completo dei server con stato, mod, proprietà |
| `POST` | `/api/servers/create` | Crea un server: `{name, kind, mc_version, loader_version}` |
| `DELETE` | `/api/servers/{id}` | Elimina definitivamente un server (rifiutato se è in esecuzione) |
| `POST` | `/api/loaders/mc-versions` | Versioni di Minecraft per un tipo: `{kind}` |
| `POST` | `/api/loaders/versions` | Build del loader: `{kind, mc_version}` |

`kind`: `vanilla` · `paper` · `forge` · `neoforge` · `fabric`.

### Ciclo di vita di un server

| Metodo | Percorso | Descrizione |
|---|---|---|
| `POST` | `/api/servers/{id}/start` · `/stop` · `/kill` | Avvia, ferma con `stop`, termina il processo |
| `POST` | `/api/servers/{id}/command` | Invia un comando alla console: `{command}` |
| `POST` | `/api/servers/{id}/eula` | Accetta la EULA di Minecraft |
| `GET` | `/api/servers/{id}/logs` | Ultime righe di console |
| `GET` | `/api/servers/{id}/metrics` | CPU e RAM del processo Java |
| `GET` | `/api/servers/{id}/disk-usage` | Byte e numero di file della cartella |

### Configurazione

| Metodo | Percorso | Descrizione |
|---|---|---|
| `PUT` | `/api/servers/{id}/info` | Nome e icona: `{name, icon}` |
| `PUT` | `/api/servers/{id}/launch` | RAM e UPnP: `{max_ram_mb, upnp}` |
| `PUT` | `/api/servers/{id}/properties` | `server.properties`: `{properties: {...}}` |
| `GET`/`PUT`/`DELETE` | `/api/servers/{id}/server-icon` | Icona del server (PNG 64×64, ridimensionata dall'app) |
| `GET`/`POST` | `/api/servers/{id}/backups` | Elenca o crea un backup del mondo |

### Mod e plugin

| Metodo | Percorso | Descrizione |
|---|---|---|
| `GET` | `/api/servers/{id}/content` | Contesto: tipo di server, cartella, versione, loader, se CurseForge è configurato |
| `POST` | `/api/servers/{id}/content/search` | Ricerca: `{provider, query, limit}` |
| `POST` | `/api/servers/{id}/content/versions` | Versioni di un progetto: `{provider, project_id}` |
| `POST` | `/api/servers/{id}/content/install` | Installa: `{provider, project_id, file_id}` |
| `GET` | `/api/servers/{id}/content/updates` | Aggiornamenti disponibili per le mod installate |
| `POST` | `/api/servers/{id}/content/update` | Aggiorna una mod: `{name}` |
| `POST` | `/api/servers/{id}/mods` | Carica file `.jar` (multipart) |
| `POST` | `/api/servers/{id}/mods/toggle` | Attiva/disattiva: `{name, enabled}` |
| `DELETE` | `/api/servers/{id}/mods/{name}` | Elimina un file |

`provider`: `modrinth` · `curseforge`. Le ricerche filtrano sempre per il **loader del server**; se per la versione di Minecraft non esiste nulla, la risposta contiene `relaxed_mc: true` e le build sono marcate `compatible: false`.

### Modpack

| Metodo | Percorso | Descrizione |
|---|---|---|
| `POST` | `/api/packs/resolve` | Legge un link modpack: `{url}` |
| `POST` | `/api/packs/install` | Installa: `{name, provider, project_id, file_id}` |
| `GET` | `/api/servers/{id}/updates` | Controlla aggiornamenti del modpack |
| `POST` | `/api/servers/{id}/update` | Aggiorna il modpack (backup + migrazione dati) |

### Eventi in tempo reale

```
GET /api/ws?token=<token>
```

WebSocket che inoltra gli eventi dell'app: `server-status`, `server-output`, `create-progress`, `update-progress`, `mod-progress`, `backup-progress`, `pack-updates`, `webhook-call`.

---

## Webhook

Ogni server ha un tab **Webhook** dove crearne quanti ne servono. Ogni webhook ha un id, un token e permessi indipendenti.

```
GET  /hook/<id>?token=<token>&action=say&message=Ciao
POST /hook/<id>          { "token": "...", "action": "command", "command": "time set day" }
```

I parametri si possono passare in query string, JSON o form: comodo per servizi che sanno mandare solo una GET.

### Azioni e permessi

| Azione | Parametri | Permesso |
|---|---|---|
| `say` | `message` | Messaggi |
| `command` | `command` | Comandi |
| `start` / `stop` | — | Accensione |
| `status` | — | Stato |

Regole applicate dall'host:

- Ogni azione richiede il **permesso corrispondente**, attivabile singolarmente.
- I comandi si limitano a una **lista consentita** che decidi tu (vuota = tutti i comandi permessi dal permesso "Comandi").
- Il comando `stop` è **sempre rifiutato** dall'azione `command`: fermare il server richiede il permesso di accensione, esplicito.
- Ogni chiamata viene registrata (orario, esito, IP) ed è visibile nel tab Webhook.

### Esempio: bot Discord

```js
await fetch(`http://casa-di-luca:25580/hook/${id}`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ token, action: 'say', message: `<${user}> ${text}` }),
});
```

### Esempio: comando da terminale

```bash
curl "http://127.0.0.1:25580/hook/ID?token=TOKEN&action=status"
```

---

## Note di rete

- Per impostazione predefinita l'host ascolta su tutte le interfacce: perché sia raggiungibile da fuori casa serve un port forward o l'UPnP del router. Se l'accesso passa da un tunnel o una VPN sulla stessa macchina, scegli **Solo questo PC** nelle impostazioni.
- Il traffico è **HTTP in chiaro**: adatto alla rete locale o a una VPN fra amici. Non esporre l'host su Internet senza un reverse proxy con TLS.
- Chi si collega vede e comanda solo i server dell'host, non il resto del computer.
