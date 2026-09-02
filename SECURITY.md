# Security policy

## Supported versions

| Version | Supported |
|---|---|
| 1.0.x (latest release) | Yes |
| older | No, please update |

## Reporting a vulnerability

Please do **not** open a public issue for security problems.

Use GitHub's private reporting instead: **Security → Report a vulnerability** on the repository, or this direct link:

https://github.com/Zed2101/Mineger/security/advisories/new

Include what you found, how to reproduce it and which version you tested. You will get an answer as soon as possible, normally within a week, and credit in the release notes if you want it.

## Scope

The parts of Mineger that face the network are the **host** (REST API and WebSocket under `/api`, token-protected) and the **webhooks** (`/hook/{id}`, per-hook tokens and permissions). Both are off by default. Their limits and protections are documented in [docs/API-HOST.md](docs/API-HOST.md); the 1.0.2 entry of the [changelog](CHANGELOG.md) describes the hardening done so far.

The host speaks plain HTTP and is meant for a LAN or a VPN between friends. Reports about exposing it directly to the Internet without TLS are appreciated but expected; the documentation already advises against it.

Everything else (server creation, modpack and mod installation, backups) runs locally with the user's own permissions and downloads only from the official sources (Mojang, PaperMC, Forge, NeoForge, Fabric, CurseForge, Modrinth, FTB).
