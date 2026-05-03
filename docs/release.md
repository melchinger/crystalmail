# Release-Workflow

CrystalMail wird über GitHub Actions plattformübergreifend gebaut und
als Draft-Release auf GitHub abgelegt. Der Trigger ist ein **Tag-Push**
auf `main`. Manuelle Ausführung über die Actions-UI funktioniert auch,
ist aber für reine Smoke-Tests des Workflows gedacht — produktive
Releases laufen über Tags.

## Was der Release-Workflow erzeugt

Pro Release entstehen Artefakte für drei Plattformen:

| Plattform | Artefakte                                                  |
|-----------|------------------------------------------------------------|
| Windows   | `CrystalMail_<v>_x64-setup.exe` (NSIS), `CrystalMail_<v>_x64_en-US.msi` |
| macOS     | `CrystalMail_<v>_universal.dmg` (Apple Silicon + Intel)    |
| Linux     | `crystalmail_<v>_amd64.deb`, `crystalmail_<v>_amd64.AppImage` |

Alle Artefakte landen im selben GitHub-Release als Assets.

## Release-Schritte

### 1. Versionsnummer hochziehen

Drei Stellen müssen synchron sein. Wenn divergent, sieht der Endnutzer
in den Datei-Properties andere Versionen als im Installer-Titel —
nicht kaputt, aber verwirrend.

```bash
# 0.2.0 als Beispiel
sed -i 's/"version": "0.1.0"/"version": "0.2.0"/' package.json
sed -i 's/"version": "0.1.0"/"version": "0.2.0"/' src-tauri/tauri.conf.json
sed -i 's/^version = "0.1.0"/version = "0.2.0"/' src-tauri/Cargo.toml
```

Cargo.lock zieht beim nächsten `cargo check` automatisch nach.

### 2. Smoke-Test lokal

Mindestens einmal lokal bauen, damit Du nicht erst nach 15 Min CI-Minutes
merkst dass irgendein Migration-Skript SQL-Syntax-Fehler hat:

```bash
pnpm tauri build --bundles app   # nur die Exe, ohne Installer-Wrap
```

Wenn das durchläuft + die Exe startet, bist Du release-ready.

### 3. Commit + Tag + Push

```bash
git add package.json src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json
git commit -m "Bump version to 0.2.0"
git tag -a v0.2.0 -m "CrystalMail 0.2.0"
git push --follow-tags
```

`--follow-tags` schickt Commit UND Tag in einem Aufwasch.

### 4. CI beobachten

GitHub Actions startet automatisch, sobald der Tag draußen ist. Drei
parallele Jobs (Windows, macOS, Linux), Laufzeit pro Job ~15-20 Min
beim ersten Mal (ohne Cache), danach 8-12 Min mit warmem Cache.

Im UI: **Actions** → **Release** → der laufende Run für `v0.2.0`.

### 5. Draft-Release veröffentlichen

Nach Abschluss aller drei Jobs: **Releases** → der frische Draft hat
alle Artefakte als Assets dran. Du:

1. Lädst Dir mindestens den Windows-Installer runter und installierst
   ihn auf einer sauberen Maschine (oder VM). „Funktioniert beim Bauer"
   reicht nicht.
2. Schreibst die Release-Notes (Änderungen seit letztem Tag, Breaking
   Changes, Bug-Fixes — ein Bullet-Liste reicht).
3. Klickst **Publish release**.

Ab dem Klick ist die Version öffentlich; das Tag wird unveränderlich
behandelt — Re-Tagging einer fertig-veröffentlichten Version ist tabu.

## Stolpersteine

### „Strawberry Perl wurde nicht gefunden" auf Windows

`bundled-sqlcipher-vendored-openssl` (in `Cargo.toml`) kompiliert
OpenSSL aus Source. Das mit Git mitgelieferte Perl reicht nicht
(fehlende `Locale::Maketext::Simple`). Der Workflow installiert
Strawberry Perl via `choco install strawberryperl` — wenn das mal
fehlschlägt (Repo-Outage von Chocolatey), ist `windows-latest` der
Single-Point-of-Failure. Workaround: Tag löschen, später nochmal pushen.

### „libwebkit2gtk-4.1-dev not found" auf Linux

Tauri 2 will WebKit ≥ 2.40 (Paketname `libwebkit2gtk-4.1-dev`).
Auf Ubuntu 22.04 ist das im `universe`-Repo, das standardmäßig
auf den GitHub-Runnern aktiv ist. Falls die Runner-Distro mal
weiter-rotiert wird (24.04 default) und das Paket umbenannt wird,
muss der Workflow nachgezogen werden.

### macOS DMG zeigt „beschädigt — in Papierkorb"

Beim ersten Mal nach Download: **Rechtsklick → Öffnen** statt
Doppelklick. Gatekeeper sieht eine unsignierte App und blockt
defaultmäßig. Ein Apple-Developer-Cert ($99/Jahr) plus Notarization
würde das beseitigen — bisher bewusst nicht eingerichtet, weil das
für ein Open-Source-Tool ohne kommerziellen Vertrieb übertrieben ist.

### Windows-Installer löst SmartScreen aus

Selbe Geschichte wie macOS. „Weitere Informationen" → „Trotzdem
ausführen". Reputation baut sich auf, je mehr User die Datei
ohne Probleme öffnen — eine Standard-EV-Cert-Sigantur ($250-500/Jahr)
würde sofort durchgehen, ist aber bisher nicht angeschafft.

## Manueller Workflow-Run (zum Testen)

In **Actions** → **Release** → **Run workflow** → Branch `main`
auswählen → **Run workflow** klicken. Das startet den Build ohne
Tag — Du bekommst ein Draft-Release mit dem Branch-Namen als Titel,
das Du im Anschluss wieder löschen kannst. Brauche ich nur nach
größeren CI-Konfigänderungen.

## Was als Nächstes ginge

1. **Code-Signing einrichten** — sobald die Cert-Frage geklärt ist,
   die entsprechenden Secrets (`APPLE_CERTIFICATE_BASE64`,
   `APPLE_TEAM_ID`, `WINDOWS_CERTIFICATE_BASE64`) in den Repo-Settings
   ablegen und im Workflow referenzieren. `tauri-action` unterstützt
   beides eingebaut.

2. **Auto-Update via `tauri-plugin-updater`** — die App fragt eine
   `latest.json` auf einer URL ab (z.B. dem GitHub-Releases-API-
   Endpoint), zieht bei neuer Version den Installer und fragt den
   User ob installiert werden soll. Plus Ed25519-Signatur-Verifikation.

3. **Linux-ARM (`aarch64`)** — derzeit nur amd64. Für Raspberry-Pi-
   oder Apple-Silicon-Linux-VMs nochmal ein Matrix-Element.
