# CrystalMail — Brand Guide

> Visueller Werkzeugkasten für die Landingpage, das App-Icon, Marketing-Materialien und die laufende UI-Erweiterung. Abgestimmt auf das bereits implementierte Crystal-Theme in `src/index.css`.

Dieser Guide ist **deskriptiv, nicht aspirational**: er hält fest, was die App heute schon ist, statt eine Marken-Vision zu erfinden, die wir noch erst umsetzen müssten. Wer eine Landingpage baut, ein neues UI-Element entwirft oder ein Pressepaket zusammenstellt, findet hier die richtigen Werte und die Begründungen dahinter.

---

## 1. Markenkern in einem Satz

> Klar wie ein Kristall. Schnell wie eine native App. Privat wie ein Notizbuch.

Das ist nicht nur Schluss-Blende einer Werbeprosa, sondern auch das visuelle Versprechen: **Klarheit, Geschwindigkeit, Diskretion**. Daraus leitet sich alles weitere ab.

- **Klarheit** → ruhige, gedämpfte Farbpalette, viel Whitespace, klare Hierarchie ohne Schnickschnack.
- **Geschwindigkeit** → minimaler visueller Lärm, kein animationsverliebtes Frontend, Fonts laden nie nach.
- **Diskretion** → kein Hype-Stil, keine grellen Kontraste, kein „dieses Werkzeug schreit nach Ihrer Aufmerksamkeit"-Verhalten.

---

## 2. Farb-System

CrystalMail benutzt eine **OKLCH-basierte Crystal-Skala** als Neutralton plus einen **kühlen Akzentblau** als einziger gesättigter Farbton. Beide sind in `src/index.css` als `@theme`-Tokens definiert.

### 2.1 Crystal-Skala (Neutral)

Eine 11-Stufen-Treppe mit konstantem Hue (250°) und steigender Chroma in mittleren Helligkeiten. OKLCH-Wahl, nicht HSL — perceptually uniform, der Sprung 200→300 sieht gleich groß aus wie 700→800. Wichtig für Listen-Hover-Effekte und kontrastreiches Dark-Mode-Design.

| Token | OKLCH | Hex (sRGB-approx) | Verwendung |
|---|---|---|---|
| `--color-crystal-50` | `oklch(0.985 0.002 250)` | `#fafafb` | Light-Mode Body-Background |
| `--color-crystal-100` | `oklch(0.96 0.004 250)` | `#f1f2f4` | Light-Mode Hover |
| `--color-crystal-200` | `oklch(0.91 0.006 250)` | `#dfe1e6` | Light-Mode Border |
| `--color-crystal-300` | `oklch(0.82 0.008 250)` | `#bfc3cb` | Disabled-Text |
| `--color-crystal-400` | `oklch(0.68 0.012 250)` | `#8d949f` | Subtle-Foreground |
| `--color-crystal-500` | `oklch(0.55 0.020 250)` | `#666e7c` | Muted-Foreground |
| `--color-crystal-600` | `oklch(0.44 0.025 250)` | `#4d5563` | (selten) |
| `--color-crystal-700` | `oklch(0.34 0.022 250)` | `#383f4b` | (selten) |
| `--color-crystal-800` | `oklch(0.24 0.018 250)` | `#262b34` | Dark-Mode Hover |
| `--color-crystal-900` | `oklch(0.16 0.012 250)` | `#191c22` | Light-Mode Text · Dark-Mode Panel |
| `--color-crystal-950` | `oklch(0.10 0.008 250)` | `#0e1014` | Dark-Mode Body-Background |

**Hue 250°** ist ein kühler, leicht ins Blaue spielender Neutralton. Bewusst kein reines Grau (langweilig) und kein warmes Off-White (weckt Assoziationen mit Papier / Notiz-Apps). Der Crystal-Ton trägt die „Glas / Klarheit"-Konnotation, ohne kalt zu wirken.

### 2.2 Akzent-Blau

Genau zwei Stufen, beide mit hoher Chroma in der Mitte des Helligkeitsbereichs. Hue 235° — etwas wärmer als reines Cyan, kein Übergang in Türkis.

| Token | OKLCH | Hex (sRGB-approx) | Verwendung |
|---|---|---|---|
| `--color-accent-500` | `oklch(0.64 0.16 235)` | `#3b8aff` | Dark-Mode Akzent |
| `--color-accent-600` | `oklch(0.54 0.16 235)` | `#0066ff` | Light-Mode Akzent |

**Sparsam einsetzen**: Submit-Buttons, aktiver Marker, ausgewählter Listeneintrag. Akzentfarbe ist die optische Aussage „hier passiert das, was Sie wollten" — wenn sie überall ist, wird sie unsichtbar. Faustregel: pro Bildschirm-Ansicht **eine** Akzent-Markierung pro logischer Aufgabe.

### 2.3 Semantische Tokens (was die Komponenten konsumieren)

Light-Mode-Defaults:

```css
--bg-base:     var(--color-crystal-50);   /* App-Body */
--bg-panel:    #ffffff;                    /* Karten, Modals */
--bg-hover:    var(--color-crystal-100);   /* Listen-Row-Hover */
--bg-selected: oklch(0.94 0.04 235);       /* Sel.-Background, leicht akzentlastig */

--fg-base:     var(--color-crystal-900);   /* Standard-Text */
--fg-muted:    var(--color-crystal-500);   /* Sekundär-Text */
--fg-subtle:   var(--color-crystal-400);   /* Hint, Helper, Counter */

--border-base: var(--color-crystal-200);   /* Standard-Trenner */
--border-soft: var(--color-crystal-100);   /* feine Linien innerhalb von Karten */

--accent:      var(--color-accent-600);
```

Dark-Mode automatisch via `@media (prefers-color-scheme: dark)`. Tokens kippen, Komponenten ändern sich nicht.

### 2.4 Status-Töne (für Badges, Marker, Banner)

Diese Töne sind **nicht** im Theme als CSS-Variablen, weil sie kontextspezifisch verwendet werden. Wer sie nutzt, paste-tippt sie inline im React-Code. Konsistenz garantiert dieser Brand-Guide.

| Status | Background (15 % Alpha) | Foreground | Wo |
|---|---|---|---|
| **Erfolg** (ok / archive-Aktion) | `rgba(34, 197, 94, 0.15)` | `#22c55e` | Audit-Log "OK", Action-Badge "Archive" |
| **Warnung** (move / pending) | `rgba(245, 158, 11, 0.15)` | `#f59e0b` | Action-Badge "Move", Pre-Apply-Hinweise |
| **Fehler** (delete / failed) | `rgba(239, 68, 68, 0.15)` | `#ef4444` | Action-Badge "Delete", Audit-Log "Fehlgeschlagen", Sterne der gefährlichen Aktionen |
| **Info / Confirm** (run-workflow confirm) | `rgba(59, 130, 246, 0.15)` | `#3b82f6` | Action-Badge "Confirm" |
| **Spezial** (dry-run, training) | `rgba(168, 85, 247, 0.15)` | `#a855f7` | Trockenmodus-Pill, Training-Markierung |
| **Skipped / Neutral** (audit-skip) | `rgba(168, 162, 158, 0.18)` | `#a8a29e` | Audit-Log "Übersprungen" |

Diese Palette ist **bewusst aus den klassischen Tailwind-500-Tönen** gewählt — die kennen User unbewusst aus tausend anderen Apps; Code-Reviews lesen sich flüssig. Akzent-Blau ist die einzige Eigenkreation.

### 2.5 ⭐ Flagged-Star

Eigene Farbe: `#f59e0b` (gleicher Ton wie Warning). Bewusst nicht Crystal-Blue, weil der Stern als emotionales „das ist mir wichtig"-Signal wirkt — Wärme schlägt Coolness in dem Kontext.

---

## 3. Typografie

### 3.1 Font-Stack

```css
--font-sans: "Inter", -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto,
  "Helvetica Neue", Arial, sans-serif;
```

**Inter** als Primary, weil:
- Open-Source unter SIL Open Font License (kein Lock-in)
- Ausgezeichnetes Hinting für kleine Größen (11-12px Listen-Text)
- Tabular-Variant verfügbar für Datums-Spalten
- Existiert als variable font → eine Datei für alle Weights

**Lokal ausliefern, nie nachladen.** Inter wird mit der App gebündelt (oder über System-Inter wenn vorhanden). Keine Google-Fonts-Verbindung. Auf macOS und iOS fällt die Kette auf `-apple-system` zurück, was nahtlos San Francisco rendert.

### 3.2 Typografische Skala

Alle Größen in `rem`-Vielfachen, Tailwind-Token für Einfachheit:

| Verwendung | Klasse | Größe | Weight |
|---|---|---|---|
| Headline (Settings-Sektion, Modal-Titel) | `text-sm font-semibold` | 14 px | 600 |
| Body (Mailliste, Reader-Body, Forms) | `text-sm` | 14 px | 400 |
| Sekundär-Text (Hints, Counter) | `text-xs` | 12 px | 400 |
| Metadaten (Badges, Datum, Account) | `text-[11px]` | 11 px | 400-500 |
| Mikro (Audit-Timestamp, Helper-Glyph) | `text-[10px]` | 10 px | 400 |

**Keine Display-Größen oberhalb von ~28 px im UI.** Das App-Fenster ist ein Werkzeug, kein Magazin. Headlines auf der Landingpage dürfen größer (z. B. 48-60 px), aber innerhalb der App bleibt's konservativ.

### 3.3 Mono für Code

`ui-monospace, SFMono-Regular, Menlo, Consolas, monospace` — wird verwendet für Mail-Header in Dev-Tools, Skript-Trace-Ausgaben, Audit-Log-Snippets. Innerhalb des Mail-Bodys nur, wenn der Sender es so wollte.

---

## 4. Logo & Wordmark

Aktuell genutzt: **`crystalmail-logo.png`** (128 px) als Splash-Icon und als Idle-State im Reader.

### 4.1 Konzept

Das Logo zeigt **einen abstrakten Briefumschlag-Stein**: ein geometrisches Element, das gleichzeitig als Mail-Symbol UND als Kristall lesbar ist. Hue + Chroma orientieren sich am Akzent-Blau (oklch ~0.5 0.16 235), das Logo ist also **monochrom blau** auf transparentem Hintergrund.

Wenn ein neues Logo gestaltet wird, sind das die Constraints:
- **Eine Farbe** (Akzent-Blau), keine Verläufe
- **Symmetrisch** oder mindestens visuell ausbalanciert — wirkt verlässlich
- **Lesbar bei 16×16** (Tray-Icon, Tab-Favicon) und bei 512×512 (App-Icon im macOS Dock)
- **Geometrische Grundformen**, keine handgezeichneten Striche

### 4.2 Wordmark

Klartext: **„CrystalMail"** im Inter Bold (700) auf der `--fg-base`-Farbe. Kein Kerning-Geschoss, keine ligaturierten Buchstaben.

Bei der Landingpage-Hero kommt der Wordmark **rechts neben dem Logo** auf gleicher Baseline. Auf engerem Raum (Tab-Titel, Dock) reicht das Logo allein.

### 4.3 Verbotene Logo-Manipulationen

- Keine farblichen Abweichungen vom Akzent-Blau (außer "vollflächig schwarz für Print" oder "vollflächig weiß auf dunklem Untergrund")
- Keine Schatten, kein Bevel, kein 3D
- Keine Rotation um eine Achse — das Symbol ist horizontal aufgesetzt
- Keine Dehnung / Stauchung — Aspekt-Ratio bleibt 1:1

---

## 5. Komponenten-Sprache

CrystalMail's UI hat ein sehr konsistentes visuelles Vokabular. Wer ein neues Modal, einen neuen Picker oder ein neues Badge baut, hält sich an diese Patterns.

### 5.1 Modal

```
┌────────────────────────────────────────┐
│ Titel                              ✕   │  ← border-b var(--border-soft)
├────────────────────────────────────────┤
│ Body — flex-col gap-3 px-4 py-4        │
│                                        │
│ [Form-Felder, Listen, Hinweise]        │
│                                        │
├────────────────────────────────────────┤
│                  [ Abbrechen ] [Save]  │  ← border-t var(--border-soft)
└────────────────────────────────────────┘
```

- Backdrop: `rgba(0, 0, 0, 0.55)`. Klick außerhalb schließt — außer das Modal hat ungespeicherte Änderungen.
- Border-Radius: `rounded-xl` (12 px).
- Border: 1 px `var(--border-base)`, leichter Schatten (`shadow-xl`).
- Maximale Breite: `max-w-md` für Eingabe-Dialoge, `max-w-2xl` für Editoren.
- Esc → schließt; Strg/⌘+Enter → submitiert.

### 5.2 Badge

```
[ AUTO ]  [ TROCKENMODUS ]  [ → ARCHIV ]
```

- Klein, `text-[10px]` mit `uppercase tracking-wider`
- `rounded` (4 px) — leicht abgerundet, nicht pill-förmig
- Padding: `px-1.5 py-0.5`
- Background: 15 %-Alpha-Variante des passenden Status-Tons
- Foreground: 100%-Variante des Status-Tons (z. B. `#22c55e` bei Erfolg)
- Bei Klick: nichts. Badges sind Anzeigen, keine Buttons.

### 5.3 Button

Drei Varianten:

**Primary** (Submit):
```css
borderColor: var(--border-base);
background: var(--accent);
color: var(--bg-panel);  /* invertiert */
```

**Secondary** (Cancel, neutrale Aktion):
```css
borderColor: var(--border-base);
background: transparent;
color: var(--fg-muted);
```

**Destructive** (Delete, irreversibel):
```css
borderColor: var(--border-soft);
color: #ef4444;
background: transparent;
```

Disabled state: `opacity: 0.6`. Niemals `pointer-events: none` ohne `cursor: not-allowed`.

### 5.4 Listen-Row (Mailliste, Audit-Log, Auto-Regeln)

```
┌─ ☐ ─ Avatar ─ Subject ─ ↩ 📎 ⏱ ─ ★ ─┐  ← hover: bg-hover, selected: bg-selected
└──────────────────────────────────────┘
```

- Höhe: ~56 px für Mail-Listen, ~36 px für kompakte Listen
- Border-Bottom `var(--border-soft)` zwischen Rows
- Hover-Background: `var(--bg-hover)`, Übergang `bg-color 80ms`
- Selected-Background: `var(--bg-selected)` (leicht akzentlastig)
- Glyph-Reihenfolge ist bewusst: **Status (↩📎)** | **Tag (⏱)** | **Stern (★)**, jeweils gemutet außer Stern (Wärme)

### 5.5 Iframe für Mail-Body

Eigenes Vokabular, sandboxed:
- Background: `#ffffff` (Light) bzw. `#1a1a1c` (Dark — leicht heller als Body)
- Body-Padding: `1rem 1.25rem`
- Schrift: `14px/1.55` auf `-apple-system, …`
- `img { max-width: 100%; height: auto }` — verhindert Layout-Sprünge bei breiten Sender-Bildern
- Blockquotes: Linker 3-px-Border in `--fg-subtle`-Farbton

---

## 6. Iconographie

### 6.1 System

Im UI **werden Emoji als Glyphen verwendet**, nicht ein Icon-Set. Bewusst:

- ✕ Schließen
- 📎 Anhang
- ↩ Beantwortet
- ↪ Weitergeleitet
- ★ Markiert
- 👁 Trockenmodus / „beobachte nur"
- ⏱ Aktive Zeitsteuerung
- ⏰ Überfällig
- ▶ Befehl wird ausgeführt
- ✔ Erfolg
- ✘ Fehler
- · Trenner zwischen Inline-Metadaten
- → Aktion-Pfeil ("→ Archiv")

**Warum Emoji statt einer Icon-Library?**
- Keine zusätzliche Library im Bundle (CrystalMail-Bundle ist 20 MB statt 30+)
- System-Renderer macht das Hinting; Inter-Glyph-Kohärenz bleibt
- Konsistent über alle Plattformen — die User sieht überall denselben „⏱"
- Emoji haben semantische Wiedererkennbarkeit, eingespielt durch jahrelange Browser-Nutzung

**Ausnahmen** (wo SVG sinnvoller wäre):
- App-Icon → SVG/PNG
- Splash-Spinner → CSS-Border-Animation
- Avatar-Initialen → ein eigenes Span mit Background-Color aus dem Account-Color

### 6.2 Avatar / Account-Color

Jedes Konto hat ein `color`-Feld (Hex-String, vom User wählbar). In Listen wird das Avatar als 24×24-Quadrat mit dem Account-Color als Background + den Initialen des Anzeigenamens in `#ffffff` (oder `#000000` je nach Background-Helligkeit, Auto-Berechnung).

**Default-Palette** für neue Konten — bei der Account-Erstellung als Quick-Picker:

| Slot | Hex |
|---|---|
| Privat | `#3b82f6` (cool blue) |
| Arbeit | `#22c55e` (forest green) |
| Familie | `#a855f7` (warm violet) |
| Newsletter | `#f59e0b` (amber) |
| Backup | `#a8a29e` (warm grey) |

User kann jederzeit überschreiben.

---

## 7. Bildwelt für Marketing

Was auf der Landingpage als Heldenbild oder Inline-Screenshot erscheint, sollte folgenden Kriterien folgen:

- **Echte App-Screenshots, keine Mockups.** Die App ist ehrlich; das Marketing soll's auch sein.
- **Dark-Mode für Hero-Bilder bevorzugen.** Wirkt premium, hebt das Akzent-Blau besser hervor, frisst weniger Aufmerksamkeit als grelle Light-Mode-Großflächen.
- **Echte Mail-Inhalte unkenntlich machen** — Subjects bleiben echt-wirkend, Sender-Adressen werden zu `name@example.com`, persönliche Infos im Reader-Body sind weg.
- **Cropping**: zeigt Mail-Liste links, Reader rechts. Sidebar (Account-Liste) optional, abhängig vom Bildausschnitt-Zweck.

**Bewusst nicht**:
- Glamour-Shots auf MacBook-Aufnahmen mit Lifestyle-Hintergrund (zu klischeehaft)
- iOS-/Android-Mockups (CrystalMail ist Desktop-only)
- Personen in Bildern (lenkt vom Produkt ab)
- Stock-Photos generischer Büros

---

## 8. Stimme & Schreibhaltung

### 8.1 Tonalität

**Sachlich, ehrlich, konkret.** Nicht verspielt-distanziert, nicht enthusiastisch-übertrieben. Wenn etwas nicht da ist, sagen wir's. Wenn etwas Trade-offs hat, nennen wir sie.

Beispiele für gute Tonalität:

✓ „Sweeper räumt nach Frist auf, oder bei Trockenmodus gar nicht."
✗ ~~„Magisch räumt CrystalMail Ihre Mailbox auf, ohne dass Sie etwas tun müssen."~~

✓ „Mehrere Konten gleichzeitig — IDLE pro Konto, eine Suche darüber."
✗ ~~„Erleben Sie Mail neu mit Multi-Account-Power!"~~

✓ „Wenn Sie den Schlüssel verlieren: keine Wiederherstellung. Bug oder Feature, je nach Sicht."
✗ ~~„Ihre Daten sind 100% sicher dank modernster Verschlüsselung."~~

### 8.2 Anrede

**Sie**, durchgehend. Das Tool macht echte Sicherheits-Versprechen — Per-Du-Tonalität wirkt unangemessen leichtfertig.

In Code-Kommentaren und Dev-Docs: Du / wir, casual, weil das die interne Kommunikation an die Entwicklung ist.

### 8.3 Fachbegriffe

CrystalMail spricht mit Power-Usern. Das heißt:

- IMAP, SMTP, IDLE, SQLCipher, FTS5 — alles ohne Abkürzungs-Auflösung. Wer nicht weiß was IMAP ist, ist nicht Zielgruppe.
- ABER: jeder Begriff darf bei seinem ersten Gebrauch in einem Kontext stehen, der ihn implizit erklärt. „CrystalMail spricht IMAP direkt mit Ihrem Server" sagt genug.
- Anglizismen sind ok (Workflow, Sweeper, Picker), aber nicht überall — „Auto-Regel" statt „Auto-Rule" im UI.

---

## 9. Anwendungsbeispiele für die Landingpage

### 9.1 Hero-Sektion

```
[Dark-Mode-Background, var(--color-crystal-950)]
                                                        
[Logo 80×80 in Akzent-Blau]
                                                        
   Mail-Client, der Ihnen gehört.                       
   ────────────────────────────                         
                                                        
   CrystalMail synchronisiert Ihre IMAP-Postfächer,    
   lernt aus Ihrem Verhalten und automatisiert das    
   Aufräumen — alles lokal verschlüsselt auf Ihrem    
   Rechner.                                            
                                                        
   🔒 Lokal verschlüsselt   ⚡ IDLE-Echtzeit  🤖 Pi-KI 
                                                        
   [ Download für Windows ]  [ Source bauen ]          
```

Akzent-Blau-Button (Primary) für Download, Secondary für Source. Schrift Inter, Headlines 48 px, Body 18 px.

### 9.2 Feature-Block (Beispiel: Auto-Regeln)

```
[Light-Mode, var(--color-crystal-50)-Background]

[Screenshot der Auto-Regel-Liste, Dark-Mode-App]
                                                        
   Auto-Regeln                                          
   ──────────                                           
                                                        
   Mails sortieren, ohne dass Sie sortieren.           
                                                        
   • Pattern-Bedingungen (Absender, Domain, Subject,    
     Anhang-Endung)                                     
   • Aktion: Archive / Delete / Move / Workflow         
   • Verzögerung von 0 Min bis 30 Tagen                 
   • Trockenmodus zum Beobachten                        
   • Audit-Log jeder Ausführung                         
                                                        
   → Use-Case: Newsletter-Cleanup nach 10 Min           
```

Lila Akzent-Pill für „Trockenmodus", grünes Badge für „Archive" — das User-bekannte Vokabular der App.

---

## 10. Was NICHT in Marketing-Materialien gehört

- **„Revolutionär", „bahnbrechend", „Game-Changer"** — Hype-Vokabular, passt nicht zum Tool
- **Stock-KI-Bilder** (humanoide Roboter, glühende Datennetze) — pi ist eine ehrliche Code-Integration, nicht Magie
- **Vergleichstabellen mit Mitbewerbern in Sterne-Form** — sieht nach Werbeagentur aus
- **Animationen, die ablenken** — die App hat keine, die Landingpage soll auch keine haben (außer feinster CSS-Hover-Effekt)
- **Pop-up-Cookie-Banner** — die Landingpage **darf keine Cookies setzen**, das wäre die ironische Selbst-Demontage

---

## 11. Asset-Checkliste

Was als Auslieferungs-Material vorhanden sein sollte:

- [ ] **App-Icon** als SVG-Master + PNG in 16/32/64/128/256/512/1024 px
- [ ] **Tray-Icon** in 16 + 32 px (monochrom)
- [ ] **Splash-PNG** (gibt's schon: `crystalmail-logo.png` 128 px)
- [ ] **Open-Graph-Bild** 1200×630 px für Social-Share
- [ ] **Twitter-Card-Bild** 1200×675 px (kann gleich OG sein)
- [ ] **Favicon** für die Landingpage (16 + 32 + Apple-Touch 180)
- [ ] **3 App-Screenshots** Dark-Mode für Landingpage (Inbox, Pre-Apply-Dialog, Audit-Log)
- [ ] **3 App-Screenshots** Light-Mode für Settings-Seiten
- [ ] **Icon-Spezifikationen-PDF** für Drittanbieter (App-Stores, Tauri-Bundler)
- [ ] **Inter-Font-Subset** (nur Latin) für die Landingpage, lokal gehostet

---

## 12. Quick-Reference: die wichtigsten Werte

Wenn jemand mit dem Brand Guide unter der Achsel zur Landingpage geht, sind das die fünf Werte, die er im Schlaf können muss:

1. **Akzent-Blau Light-Mode**: `oklch(0.54 0.16 235)` ≈ `#0066ff`
2. **Akzent-Blau Dark-Mode**: `oklch(0.64 0.16 235)` ≈ `#3b8aff`
3. **Body-Background Light**: `oklch(0.985 0.002 250)` ≈ `#fafafb`
4. **Body-Background Dark**: `oklch(0.10 0.008 250)` ≈ `#0e1014`
5. **Font**: Inter, 14 px Body, 48 px Headlines auf der Landingpage

Alles andere ist Verhandlungsspielraum oder kommt aus den `--bg-/--fg-/--border-`-Tokens, die sich von selber auflösen.

---

*Stand des Dokuments: synchron mit dem `@theme`-Block in `src/index.css`. Bei Änderungen am Theme bitte hier nachziehen, sonst läuft das Marketing-Material aus dem visuellen Tritt mit der App.*
