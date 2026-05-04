# CrystalMail

Clean, fast, local-first IMAP mail client with `pi`-powered AI features.

Desktop app built on **Tauri v2 + Rust + React + TypeScript + Vite**.
Governance scaffolded with [agentum](../agentum); AI integration pattern
ported from [mila](../mila).

## Status

Early scaffolding. See `docs/plan.md` (or the approved plan at
`~/.claude/plans/ich-bin-mit-den-graceful-noodle.md`) for the finalized
architecture decisions (AD #1–#15).

## Install (End-User)

Pre-built installers are published on the
[Releases](https://github.com/melchinger/crystalmail/releases) page.

| Platform | Download                                           |
|----------|----------------------------------------------------|
| Windows  | `CrystalMail_<v>_x64-setup.exe` (NSIS) oder `.msi` |
| macOS    | `CrystalMail_<v>_universal.dmg` (Apple Silicon + Intel) |
| Linux    | `crystalmail_<v>_amd64.AppImage` (empfohlen) oder `.deb` |

⚠️ Builds sind nicht code-signiert — beim ersten Start einmalig
SmartScreen-/Gatekeeper-Warnung wegklicken.

### Linux: AppImage vs. `.deb`

Auf Linux gibt's zwei Pakete. **AppImage ist der empfohlene Weg** — bringt
seine WebKit-Runtime selbst mit, läuft auf jeder gängigen Distro.

```bash
chmod +x crystalmail_*_amd64.AppImage
./crystalmail_*_amd64.AppImage
```

Falls AppImage nicht startet, fehlt meist `libfuse2`:
```bash
sudo apt install libfuse2     # Ubuntu/Debian/Mint
```

Das **`.deb`-Paket** braucht `libwebkit2gtk-4.1-0` (Tauri-2-Standard) und ist
nur auf neueren Distros installierbar:

| Distro                  | `.deb` läuft? |
|-------------------------|---------------|
| Ubuntu 24.04+           | ✓             |
| Ubuntu 22.04 LTS        | ✓ (`universe`-Repo aktivieren) |
| Debian 12 (Bookworm)    | ⚠️ via `backports` |
| Debian 11 (Bullseye)    | ✗ — AppImage nutzen |
| Linux Mint 21+          | ✓ (`universe`)|
| Linux Mint 20.x         | ✗ — AppImage nutzen |
| Fedora 38+ / Arch       | ✓ (eigene Build, AppImage geht aber auch) |

Wenn `apt install` mit `Abhängigkeit nicht erfüllbar:
libwebkit2gtk-4.1-0` abbricht, ist Deine Distro zu alt für das `.deb`. Nimm
die AppImage — ohne Aufwand.

## Build from Source

### Requirements

- Node.js 20+
- Rust stable (via [rustup](https://rustup.rs))
- Tauri v2 platform prerequisites ([Windows](https://v2.tauri.app/start/prerequisites/#windows) / macOS / Linux)
- A local `pi` binary on `PATH` (configurable later via Settings)

### Dev

```bash
npm install
npm run tauri dev
```

`npm run tauri dev` runs `cargo tauri dev`, which in turn spawns Vite
(`beforeDevCommand`) and the Rust backend.

### Release Build

```bash
npm run tauri build
```

Produces installers in `src-tauri/target/release/bundle/`. See
[`docs/release.md`](docs/release.md) for the full release workflow
(version bump, tag push, draft-release flow).

## Layout

```
crystalmail/
├── src/                      # React frontend (Vite)
│   ├── App.tsx
│   ├── main.tsx
│   ├── i18n.ts
│   ├── index.css
│   └── locales/de.json       # German strings; further locales add siblings
├── src-tauri/                # Rust backend (Tauri v2)
│   ├── src/
│   │   ├── main.rs           # Tauri bootstrap, plugins, state
│   │   ├── state.rs          # AppState + PiConfig
│   │   ├── commands/         # Tauri `#[command]` adapters
│   │   ├── domain/           # Account, Message, Auth (pure types)
│   │   ├── application/      # Orchestration (use-cases)
│   │   ├── infrastructure/   # DB, account-actors, event bus
│   │   └── llm/              # pi RPC client (ported from mila)
│   ├── capabilities/default.json
│   ├── tauri.conf.json
│   ├── build.rs
│   └── Cargo.toml
├── index.html                # Vite entry
├── package.json
├── vite.config.ts
├── tsconfig.json
├── AGENTS.md                 # Governance rules for AI coding sessions
├── LICENSE                   # MIT
└── docs/                     # review-checklist, security-checklist
```

## Architecture Pillars

1. **Offline-first**: Encrypted SQLite (SQLCipher) is the source of truth. UI
   never talks to IMAP directly — it reads from the DB.
2. **Actor-per-Account**: Each IMAP account runs in its own `tokio::spawn`-ed
   task holding its IDLE connection. They send write commands via `mpsc` to a
   single `db_writer` actor, so SQLite never sees concurrent writers.
3. **SQLite FTS5** for full-text search (works transparently through SQLCipher;
   no separate index directory to secure).
4. **pi as the only LLM adapter**: no direct Ollama HTTP calls from Rust.
   Mail-specific AI use-cases (smart search, auto-tagging, summarization,
   compose-assist) flow through the ported `pi_rpc.rs`.
5. **Internal event bus** (`tokio::broadcast`): mail lifecycle events
   (`MailReceived`, `MailSent`, `MailArchived`, `MailTagged`) fan out to AI,
   notifications, and future plugins.

See [AGENTS.md](AGENTS.md) for the governance ruleset before making changes.

## License

[MIT](LICENSE)
