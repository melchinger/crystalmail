# CrystalMail — Landingpage-Brief

> Ein lokaler, schneller, KI-fähiger Mail-Client. Ohne Cloud-Zwang. Ohne Tracking. Ohne Telemetrie. Ihre Mails bleiben bei Ihnen.

Dieses Dokument ist die inhaltliche Grundlage für die CrystalMail-Landingpage. Es ist als Bauplan formuliert, nicht als fertige Werbeprosa — alle Abschnitte hier sollten eins zu eins als Hero, Feature-Block oder FAQ-Eintrag übernehmbar sein, mit kleinen Anpassungen für Ton und Layout.

---

## 1. Hero / Tagline

**Headline:**
> Mail-Client, der Ihnen gehört.

**Sub-Headline (eine Zeile):**
> CrystalMail synchronisiert Ihre IMAP-Postfächer, lernt aus Ihrem Verhalten und automatisiert das Aufräumen — alles lokal verschlüsselt auf Ihrem Rechner.

**Drei Punkte unter der Headline:**
- 🔒 Lokal verschlüsselt (SQLCipher) — kein Server liest mit
- ⚡ Echtzeit-IMAP via IDLE, nahezu sofortige Mails
- 🤖 KI-Automatisierung mit lokalen Modellen (oder gar keiner)

**CTA:** „Jetzt herunterladen — Windows, macOS, Linux"

---

## 2. Warum es CrystalMail gibt (Problemstellung)

**Drei kurze Pitches**, die der „Why"-Block unter dem Hero spielt:

### „Thunderbird ist mir zu alt, Outlook zu Cloud, Mailspring zu still."
Die etablierten Mail-Clients haben jeweils ein Problem. Outlook diktiert Cloud-Lock-in. Thunderbird tritt auf der Stelle. Webmail kennt Ihre Mails besser als Sie selbst. CrystalMail ist die Antwort: ein Desktop-Client, der schnell ist, lokal arbeitet und die KI mitbringt — ohne dass Sie dafür Ihre Postfächer bei einem Anbieter parken müssen.

### „Ich will Mails sortieren, ohne sie zu sortieren."
Filterregeln in den meisten Mail-Clients sind eine Plage: starr, schwer zu testen, kein Audit. CrystalMail-Auto-Regeln matchen, taggen und räumen auf — mit Verzögerung in Minuten oder Tagen, mit einem Trockenmodus zum Beobachten, mit einem vollständigen Verlauf. Die KI schlägt sogar passende Regeln vor, wenn Sie ihr 3 Beispiel-Mails markieren.

### „Mein Postfach ist privat — also auch lokal."
Mails, die Sie lesen, gehen nicht in die Cloud. Tracking-Pixel sind standardmäßig blockiert. Anhänge öffnen sich nicht von allein. Die lokale Datenbank ist mit einem Schlüssel verschlüsselt, der nur in Ihrem OS-Keyring liegt. Wenn der Festplattenlaufwerk auf den Wertstoffhof geht: ein Schlüssel verloren = alle Mails verloren. Genau so soll's sein.

---

## 3. Hauptfeatures (Marketing-Block)

Layout: 6er-Grid mit Icons + 1-Satz-Beschreibung. Reihenfolge nach Wichtigkeit für die typische Zielgruppe (Power-User, die selbst entscheiden, was passiert).

| Feature | Pitch |
|---|---|
| 🔒 **Lokal verschlüsselt** | SQLCipher-Datenbank, Master-Key im OS-Keyring. Nichts liegt im Klartext auf der Platte. |
| ⚡ **Multi-Account, Unified Inbox** | Beliebig viele IMAP-Konten. Eine Inbox-Ansicht, eine Sucheingabe. Auto-Discovery der Spezialordner. |
| 🤖 **KI-Automatisierung** | Lokales pi-Modell (oder Ihr Wunschprovider) schlägt Filter vor, extrahiert Kontaktdaten aus Signaturen, klassifiziert Spam. Lässt sich komplett deaktivieren. |
| 📋 **Workflows + Auto-Regeln** | Multi-Schritt-Pipelines (Skripte, Datei-Export, Anhänge speichern) plus Pattern-Regeln mit Verzögerung, Trockenmodus und Audit-Log. |
| 🎯 **Tracking-Pixel-Schutz** | Remote-Bilder pro Mail blockiert, mit User-Override pro Absender oder Domain. Keine 1×1-Phone-Home-Pixel ohne Ihre Zustimmung. |
| 🛠 **Eigene Skripte** | Python-Skripte in Workflows einbinden — vollständig argparse-aware. Argumente per Dialog, Template-Variablen oder festem Wert. |

---

## 4. Tiefere Feature-Kapitel

Das hier sind die Abschnitte, die unter dem Pitch-Grid jeweils einen eigenen Streifen / eine eigene Sektion bekommen.

---

### 4.1 Privatsphäre & Sicherheit

**Headline:** Ihr Postfach gehört Ihnen — auch auf der Festplatte.

CrystalMail ist als Privacy-First-Client gebaut. Konkret heißt das:

- **Verschlüsselung at rest** mit SQLCipher (AES-256). Der Schlüssel wird beim ersten Start aus dem OS-RNG generiert und im OS-Keyring gespeichert (Windows Credential Manager, macOS Keychain, Linux Secret Service / KWallet). Kein Plaintext-Fallback.
- **Geschützte HTML-Ansicht.** Mail-Bodies werden in einem Sandbox-Iframe (`sandbox="allow-scripts"`, kein `allow-same-origin`) gerendert. Eine Content-Security-Policy in zwei Schichten verhindert, dass Sender-HTML Skripte ausführt, externe Ressourcen nachzieht oder das App-Fenster manipuliert.
- **Tracking-Pixel-Schutz.** Externe Bilder sind standardmäßig blockiert. Pro Mail können Sie sie nachladen; pro Absender oder Domain können Sie persistente Whitelist-Einträge setzen, falls Sie z. B. einen bestimmten Newsletter-Anbieter generell freigeben wollen.
- **Anhänge bleiben Anhänge.** Inline-Bilder werden zu `data:`-URLs und durchlaufen nie das Netzwerk. Erst der Klick auf „Öffnen mit Standard-App" lässt Datei + Programm aufeinander treffen — und auch dann landet die Datei nur in einem temporären Per-Mail-Unterordner, nicht im Download-Verzeichnis.
- **Keine Telemetrie.** Kein Analytics, kein Crash-Reporter, kein Update-Ping. Updates laufen über GitHub-Releases, manuell gestartet.
- **Code-Audit-tauglich.** Komplett Open Source unter MIT, in Rust + TypeScript geschrieben. Keine Closure-Bibliotheken, keine kompilierten Drittanbieter-Blobs.

---

### 4.2 Auto-Regeln: das Herzstück der Automatisierung

**Headline:** Mails sortieren, ohne dass Sie sortieren.

Die meisten Mail-Clients haben Filterregeln. CrystalMail hat ein vollständiges Regel-System mit Zeit-Achse, Trockenmodus und Audit-Log. Eine Regel besteht aus:

**Bedingungen** — UND-verknüpft, mehrere Regeln für ein Workflow ODER-verknüpft:
- Absender-Adresse exakt
- Absender-Domain exakt (oder Liste mehrerer Domains)
- Betreff enthält
- Anhang-Endung (auch zusammengesetzt: `tar.gz`)

**Aktionen** — was bei einem Treffer passiert:
- **Workflow ausführen** (mehrstufig: Anhänge speichern, Body als Markdown, Skript laufen lassen)
- **Ins Archiv verschieben**
- **In den Papierkorb** (nicht permanent — Sie können wiederherstellen)
- **In einen bestimmten Ordner verschieben**

**Verzögerung** — von 0 Minuten (sofort) bis 30 Tagen, in Minuten-Granularität:
- 0 → sofort beim Empfang handeln
- 10 → „Wenn ich's in 10 Min nicht angefasst habe, weg damit" (Newsletter)
- 1440 → „Heute war noch wichtig, morgen ins Archiv"
- 43200 → „Bestellbestätigungen einen Monat liegen lassen"

**Trockenmodus** — der Schutzmechanismus, der das ganze System sicher macht. Beim ersten Anlegen einer Regel ist Trockenmodus standardmäßig an: die Regel matcht, taggt die Mail mit einem Marker (👁) im Posteingang, aber löscht/verschiebt nichts. Sie können ein paar Tage beobachten, ob die Treffer wirklich passen — dann mit einem Klick scharf schalten.

**Audit-Log** — jeder Sweep-Versuch wird protokolliert: welche Regel, welche Aktion, welche Mail (Subject + Sender als Snapshot, auch wenn die Mail längst weg ist), Erfolg / Skip / Fehler. Sichtbar in den Settings, scrollbar, mit Filter pro Regel.

**Backfill auf bestehende Mails** — direkt nach dem Anlegen einer Regel: „247 bestehende Mails passen — auch markieren?" Klick → der Sweeper räumt nach Frist auf, oder bei Trockenmodus zeigt er nur die Marker.

**„Jetzt anwenden"-Button** — manueller Sweep-Trigger, falls Sie nicht auf den nächsten Sync-Tick warten wollen.

#### Beispiel: Newsletter-Cleanup in 10 Minuten

> Ein typischer Use-Case: Sie abonnieren einen Newsletter. Sie öffnen ihn manchmal, oft aber nicht. Was Sie nicht sehen wollen: nach einer Woche stapeln sich 50 ungelesene davon im Posteingang.
>
> Lösung in CrystalMail:
> 1. Eine Regel: *Domain ist `newsletter.example.com` → ins Archiv → Verzögerung: 10 Min → Trockenmodus.*
> 2. Eine Woche beobachten.
> 3. Trockenmodus aus.
>
> Resultat: jeder Newsletter, den Sie in den ersten 10 Minuten nicht öffnen, ist still ins Archiv gewandert. Ungelesene Newsletter sammeln sich nicht mehr; gelesene Newsletter bleiben im Archiv durchsuchbar.

---

### 4.3 Workflows: Skripte direkt aus dem Mail-Client

**Headline:** Mails als Trigger für eigene Werkzeuge.

Mit Workflows verbinden Sie CrystalMail an Ihre vorhandenen Python-Skripte. Drei Schritt-Typen:

- **Anhänge speichern** — alle Non-Inline-Attachments in einen Zielordner schreiben, optional gefiltert per Glob (`*.csv`, `*.pdf`).
- **Body als Datei** — den Mail-Inhalt als Markdown, Plaintext oder `.eml` ablegen.
- **Skript ausführen** — ein Python-Skript aus einem benannten Verzeichnis aufrufen, mit strukturierten Argumenten.

#### Skript-Argumente: drei Quellen

CrystalMail liest die Argumente Ihres Skripts via `argparse`-Inspektion automatisch aus, so dass Sie sie im Editor nicht doppelt pflegen müssen. Pro Argument wählen Sie, woher der Wert beim Lauf kommt:

- **Fester Wert** — eine vorab eingetippte Zeichenkette. Mit `$variable`-Substitution (z. B. `~/Downloads/$datetime_compact-anhang.csv`).
- **Template-Variable** — eine der 14 vordefinierten Variablen (`$subject`, `$from`, `$datetime`, …).
- **Erster Anhang vom Typ** — der erste Anhang mit passender Endung wird als Pfad eingesetzt.
- **Dialog-Eingabe** — vor jedem Lauf öffnet sich ein Dialog, in dem Sie den Wert eingeben oder aus einer Auswahlliste picken.

#### Template-Variablen für Datum & Zeit

Eine Auswahl, die Sie in jedem Argument-Wert nutzen können:

| Variable | Beispielwert |
|---|---|
| `$subject` | `Server-Wartung Mai` |
| `$from` | `wartung@dienstleister.de` |
| `$date` | `2026-05-12T14:30:00+02:00` (RFC 3339) |
| **`$datetime`** | **`2026-05-12 14:30`** (für Clockodo, Zeiterfassung) |
| `$datetime_seconds` | `2026-05-12 14:30:45` |
| `$datetime_iso` | `2026-05-12T14:30` |
| `$date_iso` | `2026-05-12` |
| `$date_de` | `12.05.2026` |
| `$datetime_compact` | `20260512-1430` (filename-safe) |
| `$time` | `14:30` |
| `$year` / `$month` / `$day` | `2026` / `05` / `12` |
| `$attachments_dir` | Pfad zum temporären Anhang-Ordner |
| `$csv` | Erster CSV-Anhang als Pfad |
| `$body_md` | Body als Markdown-Datei (wenn Schritt vorher gesetzt) |

#### Pre-Apply-Dialog

Hat ein Argument die Quelle „Dialog-Eingabe", öffnet sich vor dem Skript-Lauf ein Dialog mit einem Eingabefeld pro Argument:
- **Default-Templates werden vorausgefüllt** und Variablen wie `$subject` oder `$datetime` direkt aufgelöst — Sie sehen den finalen Wert, nicht die Variable.
- **Choice-Argumente** rendern als Dropdown mit Ihren konfigurierten Optionen.
- Tastatur: Strg/⌘+Enter zum Starten, Esc zum Abbrechen.

#### Trace im Terminal

Beim Skript-Aufruf landet im Terminal der vollständige, paste-fertige Befehl plus pro Argument eine Zeile mit Quelle und resolvtem Wert:

```
INFO workflow_script: ▶ python "D:\scripts\clockodo_entry_add.py" --alias OMC_NVBW_DNS --datetime "2026-05-12 14:30"
INFO workflow_script:   · prompt(user) → "OMC_NVBW_DNS"     param=--alias  kind=Option value_type=Choice required=true
INFO workflow_script:   · prompt(default="$datetime") → "2026-05-12 14:30"  param=--datetime
INFO workflow_script: ✔ rc=0   elapsed_ms=842
```

Workflow-Bugs („warum kam Argument X nicht durch?") werden damit zur Frage des Logs lesens, nicht des Ratens.

---

### 4.4 KI mit pi: optional, lokal, transparent

**Headline:** Ein Helfer, kein Erzieher.

CrystalMail bindet [pi](https://github.com/melchinger/mila) als KI-Harness ein. pi orchestriert lokale LLMs (Ollama, Gemma, llama.cpp) oder externe Provider — Sie wählen, was Sie wollen. Die KI-Funktionen sind durchgehend opt-in und einzeln deaktivierbar:

- **Spam-Regel-Vorschläge.** Markieren Sie 5 Spam-Mails, klicken „Regel lernen", pi analysiert die gemeinsamen Merkmale und schlägt eine deterministische Filter-Regel vor. Sie bestätigen — die Regel ist im System, ohne dass die KI bei künftigen Mails noch involviert ist.
- **Workflow-Regel-Lernen.** Selber Mechanismus für Auto-Filter: ein paar Beispiel-Mails markieren, pi findet den engsten gemeinsamen Pattern + sinnvolle Action + Verzögerung. Bei Newsletter-artigen Mustern darf pi sogar direkt eine Archive-/Delete-Aktion vorschlagen, statt einen Workflow als Vehikel zu verwenden.
- **Kontakt-Extraktion.** Beim Öffnen einer Mail mit interessanter Signatur kann pi (auf Klick) Adress- und Telefonblock extrahieren und einen Kontakt-Datensatz vorschlagen.
- **Frei-Konversation.** Ein eingebettetes Pi-Terminal erlaubt natürlichsprachliche Rückfragen zu Mails — z. B. „Fasse die letzten drei Mails von Anbieter X zusammen".

**Privacy:** alle KI-Aufrufe laufen über pi. Wenn pi auf einem lokalen Modell zeigt, verlässt nichts den Rechner. Wenn pi auf einem Provider zeigt (OpenAI, Anthropic, Mistral …), gilt deren Datenschutz — CrystalMail selbst leitet nur das durch, was Sie für die jeweilige Aktion explizit auslösen. Es gibt einen globalen **AI-Killswitch** in den Settings: ein Schalter, der jede pi-Anfrage auf der Backend-Seite blockt, falls Sie temporär offline / privat arbeiten wollen.

---

### 4.5 Performance: das Geheimnis ist nicht die KI, sondern die DB

**Headline:** Mails öffnen, bevor sie da sind.

CrystalMail fühlt sich schnell an, weil drei Architektur-Entscheidungen ineinander greifen:

- **SQLite mit WAL-Mode** als alleiniger Datenspeicher. Lese-Operationen blockieren nicht den Schreiber, der Schreiber blockiert nicht die Leser. Eine einzelne Reader-Pool-Connection per UI-Klick, ein dedizierter Writer-Actor mit Mailbox.
- **Volltext-Suche via SQLite FTS5** auf cached envelopes + bodies. „Steuer 2024" findet die Mail in <50 ms aus 100.000 archivierten Nachrichten.
- **Lazy Body-Fetch + Prefetch.** Beim Sync ziehen wir nur Header-Bytes der letzten 30 Tage. Vollständige Bodies werden im Hintergrund vorgeladen, priorisiert nach „User scrollt sich gerade darauf zu" und „Workflow-Regel könnte feuern". Klick auf Mail → Body ist meistens schon da.

**IDLE statt Polling.** Pro Konto wird eine persistente IMAP-Verbindung gehalten, die der Server bei neuer Mail aktiv pusht. Latenz vom Mail-Eintreffen bis zum Erscheinen im Posteingang: typischerweise 1-2 Sekunden. Polling ist als Fallback-Modus pro Konto wählbar; auch eine IDLE+Polling-Kombination für Provider, die IDLE nur halbgut unterstützen.

**Sieve-/Server-Filter-tolerant.** Wenn Ihr Mail-Server bei neuer Mail eigene Verschiebungen vornimmt (Sieve, Spam-Filter), erkennt CrystalMail das automatisch und räumt seine lokale Sicht entsprechend auf — keine „Phantom-Mails" im Posteingang, die beim Klicken verschwinden.

---

### 4.6 Adressbuch mit System

**Headline:** Kontakte, die mit der Mail wachsen.

Statt Sie zu zwingen, Adressbuch-Einträge manuell zu pflegen, wächst CrystalMails Kontaktliste neben Ihrer Mailnutzung mit:

- **Auto-Extraction aus Signaturen.** Beim Öffnen einer Mail mit Signaturblock fragt CrystalMail (per Klick): „Diese Daten extrahieren? Name, Firma, Telefon, Adresse?" pi macht den Heavy-Lift, Sie bestätigen.
- **Stammdaten-Edit.** Manuelle Einträge mit allen üblichen Feldern. Mehrere E-Mail-Adressen pro Person mit Primär-Auszeichnung. Tags zum Kategorisieren („Kunden", „Stammtisch", „Familie").
- **Verknüpfung in der Mail-Ansicht.** Mail vom Absender ohne Kontakt → graues Icon im Header. Mit Kontakt → Avatar. Klick → Detail-Panel mit aller Kommunikationshistorie (gefiltert nach Adressmatch).
- **Import/Export.** vCard 4.0 + CSV in beide Richtungen. Atomar — entweder läuft die ganze Datei durch oder nichts ändert sich.

---

### 4.7 Backup, das nicht auf Mails zeigt

**Headline:** Settings einmalig sichern, überall wieder einrichten.

Mail selbst liegt auf Ihrem IMAP-Server — der ist das Backup. Was CrystalMail-spezifisch ist: Konten-Konfiguration, Filter-Regeln, Workflows, Kontakte. Das ist die Settings-Backup-Schnittstelle:

- **Vollständiger Export** als JSON-Datei. Optional Passwort-verschlüsselt (Argon2id-KDF + ChaCha20-Poly1305). Eine Datei = alle Konten + Aliase + Spam-Regeln + Workflow-Definitionen + Auto-Filter + Kontakte + Tags.
- **Import** in einen neuen Rechner mit derselben Datei. Atomar — bei Fehler an einer Stelle Rollback der gesamten Operation, kein halb-importierter Zustand.
- **Keychain wird transparent mitbehandelt.** Mail-Passwörter werden separat (auf User-Bestätigung) im neuen System-Keychain abgelegt, weil sie nicht im JSON liegen sollen.

---

## 5. Was CrystalMail bewusst NICHT tut (Anti-Features)

Eine Landingpage gewinnt durch klare Negativ-Aussagen. Hier die expliziten Nicht-Features:

- **Keine Cloud.** Keine „Login mit Microsoft / Google" für die App selbst. Sie geben Ihre Mail-Credentials direkt an das jeweilige IMAP-Konto.
- **Keine OAuth-Web-Mail-Pseudo-Integration.** Die App spricht IMAP/SMTP direkt. Wenn Ihr Provider OAuth verlangt (Gmail), nutzen Sie ein App-spezifisches Passwort.
- **Keine Online-Sync zwischen mehreren CrystalMail-Installationen.** Sie installieren auf einem zweiten Rechner → Konten neu verbinden + Settings-Backup importieren. Bewusst, weil Sync-Server eine ganze Klasse von Lock-in und Sicherheits-Trade-offs mit sich bringt.
- **Keine Telemetrie, kein „Crash report bitte schicken"-Dialog.** Wenn etwas crasht, sehen Sie's. Senden Sie's per Hand, wenn Sie wollen.
- **Keine permanente Löschung per Auto-Regel.** Direkt-Aktionen verschieben höchstens in den Papierkorb. Permanent löschen bleibt eine bewusste manuelle Geste.
- **Keine Kontakt-Synchronisation mit CardDAV / Exchange.** Adressbuch ist lokal-only. Import/Export ist die Brücke.
- **Keine integrierter Kalender, keine Tasks.** CrystalMail ist ein Mail-Client, nicht eine Personal-Information-Manager-Suite. Tasks gehen Sie über andere Tools an, der Workflow-Step „Skript ausführen" kann sie integrieren falls Sie wollen.

---

## 6. Unter der Haube (für die Skeptiker:innen)

Eine Landingpage mit technischer Zielgruppe darf zeigen, was drin ist. Vorschlag für eine „Tech Stack"-Sektion:

| Komponente | Technologie |
|---|---|
| App-Shell | [Tauri 2](https://tauri.app/) (Rust + Web-Frontend, 20 MB statt 200 MB Electron) |
| UI | React 18 + TypeScript, Tailwind 4, Vite |
| IMAP | `async-imap` mit IDLE, CONDSTORE, UIDPLUS |
| SMTP | `lettre` (Submission auf Port 587/465) |
| MIME-Parser | `mail-parser` |
| Lokal-Speicher | SQLite via `rusqlite` mit SQLCipher-Verschlüsselung |
| Suche | SQLite FTS5 |
| TLS | rustls + ring (kein OpenSSL auf dem Wire) |
| Secrets | OS-Keyring via `keyring` (Windows Credential Manager / macOS Keychain / Secret Service) |
| KI-Integration | [pi](https://github.com/melchinger/mila) als RPC-Subprozess |

**Open Source unter MIT.** Code auf GitHub. Audit jederzeit möglich. Pull Requests willkommen.

---

## 7. Roadmap (sichtbar machen, ohne sich zu binden)

Eine Landingpage gewinnt durch ehrliche Roadmap. Vorschlag, kurz formuliert:

**Bereits umgesetzt (Stand jetzt):**
- Multi-IMAP, Unified Inbox, IDLE-Sync, lokale FTS5-Suche
- SQLCipher-Verschlüsselung, Tracking-Pixel-Schutz, Sandbox-Iframe für HTML-Mails
- Spam-Regeln + KI-Lernen, Auto-Filter mit Verzögerung & Trockenmodus
- Workflows mit Skript-Integration, Pre-Apply-Dialog, Audit-Log
- Adressbuch mit Auto-Extraction, vCard/CSV
- Settings-Backup verschlüsselt
- Hotkeys für Power-User
- Windows-First, Linux funktional, macOS kompiliert

**In Arbeit:**
- Kalender-Lookup (read-only, ICS-Import)
- Server-side Sieve-Editor (Sie schreiben Sieve, CrystalMail rendert die Regeln visuell)
- Mehr KI-Eingriffstiefe: Vorschläge für Zusammenfassungen, Übersetzungen on-demand
- Multi-Device-Sync von Settings (verschlüsselt, peer-to-peer, ohne Server)

**Bewusst nie geplant:**
- Cloud-Mail-Hosting
- Werbe-Integration
- Telemetrie

---

## 8. FAQ (für die untere Landingpage-Zone)

**Wie viele Konten kann ich anbinden?**
Beliebig viele. Jedes Konto behält seine eigene IDLE-Verbindung; das Limit ist Ihr System (RAM + offene TCP-Sockets, praktisch >50 möglich).

**Funktioniert das mit Gmail?**
Ja, über App-Passwörter (Google: 2FA aktivieren → App-Passwort generieren). Direkter OAuth-Login ist nicht implementiert, weil das eine Browser-Flow-Integration verlangt, die wir bewusst nicht im Mail-Client haben wollen.

**Wo liegen meine Mails?**
Lokal. `%APPDATA%\com.melchinger.crystalmail\crystalmail.sqlite` auf Windows, entsprechend auf macOS/Linux. Eine einzige verschlüsselte SQLite-Datei. Backup-fähig per Datei-Kopie (App vorher schließen).

**Was, wenn ich den Schlüssel verliere?**
Keine Wiederherstellung möglich. Der Schlüssel im OS-Keyring ist die einzige Kopie. Bei OS-Reset → DB neu aufbauen aus IMAP. Das ist kein Bug, das ist die Sicherheits-Garantie.

**Welches KI-Modell nutzt pi?**
Was Sie konfigurieren. Standardvorschlag: `qwen2.5:14b` lokal via Ollama, weil das ein guter Trade zwischen Geschwindigkeit und Strukturtreue ist. Für stärkere Aufgaben können Sie auch GPT-4o, Claude, o.ä. einbinden — das ist eine Einstellung, kein Architektur-Lock-in.

**Wie viel kostet das?**
CrystalMail ist Open Source unter MIT-Lizenz, kostenlos. Wenn Sie pi mit einem kommerziellen Provider nutzen, gelten dessen Konditionen. Ein lokales Modell hat keine variable Kosten.

**Kann ich es selbst bauen?**
Ja. `git clone`, `cargo tauri build`. Voraussetzungen siehe README. Auf Linux brauchen Sie zusätzlich `libwebkit2gtk-4.1-dev` und einen laufenden Secret-Service-Daemon.

---

## 9. Call to Action (Schluss-Block der Landingpage)

**Headline-Vorschlag:** „Probieren Sie 30 Tage. Falls's Ihnen nicht gefällt: deinstallieren, fertig."

Drei vertikal angeordnete CTAs:

1. **[Download für Windows]** — `.msi`-Installer, signiert
2. **[Download für macOS]** — `.dmg`, ad-hoc-signiert (System-Dialog wird beim Start erscheinen)
3. **[Source bauen]** — Link zur Build-Anleitung im README

Darunter klein:
> Erstinstallation? Eine kurze Anleitung führt Sie durch das Anlegen Ihres ersten Kontos. Sie geben Hostname / Port und Ihr Mail-Passwort ein — wenn der Server üblich konfiguriert ist, werden die Spezialordner automatisch entdeckt.

---

## 10. Tonalität & Schreibhinweise

Für die schreibende Person, die diesen Brief in fertige Landingpage-Prosa verwandelt:

- **Klare Aussagen, keine Marketing-Phrasen.** „Lokal verschlüsselt" ist konkret. „Ihre Daten sind sicher" wäre Wischiwaschi.
- **Direkte Ansprache, „Sie".** Power-User-Zielgruppe; per-Du wirkt zu kumpelig für ein Tool, das echte Sicherheits-Versprechen macht.
- **Vermeiden Sie KI-Hype.** „KI-gestützt" ja, aber an konkreten Use-Cases festmachen. Nicht „revolutionäres KI-Erlebnis".
- **Gegenüber Wettbewerbern fair bleiben.** Outlook / Thunderbird / Mailspring beim Namen nennen, deren Pluspunkte anerkennen, dann den eigenen Unterschied klar machen. Keine Polemik.
- **Screenshots wirken stärker als Text.** Drei Stellen, wo ein Screenshot eingeblendet werden sollte:
  1. Unified Inbox mit Auto-Rule-Markern (👁/⏰/⏱)
  2. Pre-Apply-Dialog beim Workflow-Lauf
  3. Audit-Log mit echten Einträgen

---

## 11. Begleitende Marketing-Snippets (für Social / OG / Twitter)

**OG-Description (155 Zeichen):**
> Lokaler IMAP-Mail-Client. Verschlüsselt. KI-fähig (oder nicht). Mit Workflows, Auto-Regeln, Trockenmodus, Audit-Log. Open Source, MIT.

**Twitter-Pitch (240 Zeichen):**
> Mail-Client für Leute, die Mails sortieren wollen, ohne sortieren zu müssen. Auto-Regeln mit Verzögerung in Minuten, Trockenmodus, Audit-Log. KI-Vorschläge optional, lokal. Verschlüsselt, ohne Cloud. Windows/macOS/Linux. Open Source.

**Hacker-News-Submission-Titel:**
> CrystalMail – Local-first, encrypted IMAP client with delayed-action auto-rules and pi-driven rule learning

**Show-HN-Body (4 Sätze):**
> CrystalMail ist ein Tauri-basierter Mail-Client mit SQLCipher-verschlüsselter lokaler DB, FTS5-Suche und einem Auto-Regel-System, das Aktionen mit Minuten-Verzögerung plus Trockenmodus + Audit-Log ausführt. Workflows binden Python-Skripte mit argparse-Inspektion ein; Argumente kommen aus Templates, Anhang-Patterns oder Pre-Apply-Dialogen. Spam- und Auto-Regeln können von einem lokalen LLM (via pi) aus Beispiel-Mails gelernt werden — KI komplett opt-in. Open Source unter MIT, Rust + React.

---

*Dieses Dokument ist die inhaltliche Basis. Für die finale Landingpage müssen Layout, Farb-Palette und Bildwelt (siehe `brand_guide.md`) noch übergesetzt werden.*
