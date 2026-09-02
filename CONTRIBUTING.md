# Contributing to Mineger

Thanks for taking the time. Bug reports, translations, docs and code are all welcome.

## Before you start

- **Bugs and ideas** go through the [issue templates](https://github.com/Zed2101/Mineger/issues/new/choose). Check the open issues first; a quick search saves everyone a duplicate.
- **Security problems** must not be filed as public issues. See [SECURITY.md](SECURITY.md).
- For anything bigger than a small fix, open an issue first so the approach can be agreed before you spend time on it.

## Development setup

Mineger is a [Tauri v2](https://tauri.app) app: Rust backend, plain HTML/JS frontend styled with Tailwind CSS v4. Windows is the supported target.

Requirements: Node.js 22+, a stable Rust toolchain, and the [Tauri prerequisites for Windows](https://tauri.app/start/prerequisites/) (Visual Studio C++ build tools, WebView2).

```bash
npm install
npm run tauri dev
```

`tauri dev` compiles the backend, builds the CSS and opens the app with hot reload for the frontend. In debug builds servers and settings live inside the repository (`servers/`, `src-tauri/src/data/`); release builds use the user's AppData.

How the code is organised: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). The host API and webhooks: [docs/API-HOST.md](docs/API-HOST.md).

## Tests

```bash
cd src-tauri
cargo test --lib            # fast, offline
cargo test --lib -- --ignored   # real downloads and installers, slow
```

Frontend and backend translations are checked by `cargo test --lib i18n`: missing keys or mismatched `{placeholders}` fail the build.

## Translations

Adding a language takes one JSON file:

```bash
npm run lang:new -- fr
```

That writes `src/language/fr.json` with every key ready to fill in and registers it. Regenerate the template after adding strings with `npm run lang:template`. The website has its own dictionaries under `site/i18n/`.

## Website

The site under `site/` is deployed to GitHub Pages by `.github/workflows/pages.yml`.

```bash
npm run site:watch   # Tailwind in watch mode
npm run site:og      # re-render the social card (assets/og.png)
```

Serve the folder with any static server to preview it; `release.json` is generated in CI, locally the download page falls back to the GitHub API.

## Pull requests

- Keep them focused: one fix or feature per PR.
- Commit messages and code comments in English.
- Add a line to `CHANGELOG.md` under **Unreleased** for anything a user would notice.
- Run `cargo test --lib` and `cargo fmt` before pushing.
- The CI is the reviewer's first pass; a green build helps a lot.

By contributing you agree that your work is released under the [MIT license](LICENSE).
