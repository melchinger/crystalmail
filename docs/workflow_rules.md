# Workflow-Rules: Automatik auf eingehende Mails

CrystalMail kann eingehende Mails nach frei definierbaren Regeln
automatisch behandeln: archivieren, löschen, verschieben — oder eine
mehrschrittige **Workflow**-Pipeline auf sie anwenden (Anhänge
speichern, Body exportieren, ein Python-Script anstoßen).

Dieses Dokument beschreibt die **eingehende** Seite. Die ausgehende
Seite (externe Trigger erzeugen einen Draft im Composer) steht in
[import_workflows.md](import_workflows.md) — beides heißt „Workflow",
ist aber unabhängig.

> **Sicherheits-Modell:** Auto-Aktionen schreiben nur lokal (Move,
> Archive, Delete in den Trash) oder rufen Scripts aus einem
> User-konfigurierten, allowlisteten Verzeichnis. Permanent-Löschung
> ist als Rule-Action **nicht** vorgesehen — zu unwiderruflich für
> Pattern-Match-Automatik. Jede Regel kann im **dry-run** laufen:
> taggt sichtbar, führt aber nichts aus.

---

## Zwei Bausteine: Workflow und Rule

| Begriff      | Was es ist                                                        |
|--------------|-------------------------------------------------------------------|
| **Workflow** | Geordnete Liste typisierter Schritte (Steps). Auf *eine* Mail angewendet. Manuell per Hotkey/Picker — oder von einer Rule getriggert. |
| **Rule**     | Bedingung auf eingehende Mails (Prädikate) + eine Aktion. Eine Aktion ist entweder ein direkter Befehl (Archive/Delete/Move) oder „führe Workflow X aus". |

Eine Rule ist also der *Auslöser*, ein Workflow die *Pipeline*. Für
reines Aufräumen (archivieren/verschieben) braucht man keinen
Workflow — die Rule erledigt das direkt.

---

## Workflows

Ein Workflow hat einen Namen, optional einen Hotkey, eine Liste von
Steps und ein Flag `archiveAfterSuccess` (nach erfolgreichem Lauf die
Mail ins Account-Archiv schieben). Steps laufen top-to-bottom; ein
fehlgeschlagener Step stoppt die Kette **nicht** — jeder Step liefert
ein eigenes Ergebnis, die Summe ist der Audit-Trail im Result-Dialog.

### Step-Typen

Der `Step`-Enum ist bewusst geschlossen. Neue Aktionstypen brauchen
eine Rust-seitige Implementierung — die Vertrauensgrenze bleibt im
Typsystem sichtbar statt hinter einem generischen „run beliebiges
Ding".

| Step              | Wirkung                                                                 |
|-------------------|-------------------------------------------------------------------------|
| `saveAttachments` | Schreibt jeden (nicht-inline) Anhang nach `targetDir`. Optionaler Glob-`filter` (`*.csv`, `*.pdf`). Dir wird bei Bedarf angelegt. |
| `saveBody`        | Schreibt den Mail-Body nach `path`. `format = md\|txt\|eml` wählt die Serialisierung (md = Subject + Header + Plain-Body). |
| `runScript`       | Ruft ein Python-Script aus dem konfigurierten Script-Verzeichnis. `script` ist nur der Dateiname; `parameters` ist ein strukturierter argv-Builder. |

### Template-Variablen

In String-Feldern (z.B. `saveAttachments.targetDir`) werden Variablen
als `$name` referenziert. In `ScriptParam` (Quelle `template`) nur der
nackte Name.

| Variable           | Bedeutung                                                                 |
|--------------------|---------------------------------------------------------------------------|
| `$subject`         | Betreff aus dem Envelope                                                   |
| `$from`            | Absender-Adresse                                                          |
| `$date`            | Mail-Datum                                                                 |
| `$attachments_dir` | Pfad zu einem frischen Temp-Dir mit **allen** Anhängen der Mail (lazy befüllt) |
| `$csv`             | Pfad zur **ersten** `.csv`-Anlage, falls vorhanden                         |
| `$body_md`         | Pfad zu einer Markdown-Datei mit dem Body (gefüllt, wenn vorher ein `saveBody`-Step lief) |
| `$body_essentials` | Body-**Inhalt** als ein String, Signatur/zitierte Antwort bereits gestrippt — gedacht zum direkten Durchreichen an ein Script (z.B. Ticket-Beschreibung) |

Lazy heißt: `$attachments_dir`/`$csv` werden nur entpackt, wenn ein
Step sie referenziert — und nur einmal, egal wie viele Steps zugreifen.

### runScript: Parameter

`parameters` modelliert die argparse-Schnittstelle des Scripts. Jeder
Eintrag ist ein Parameter mit `kind` (`positional`/`option`/`flag`),
`cliName` (`--output-dir`, `-v`, oder `input` für Positional) und einer
`source`, die zur Laufzeit den konkreten Wert liefert:

| Source           | Liefert                                                                   |
|------------------|---------------------------------------------------------------------------|
| `fixed`          | Einen festen, in den Settings getippten String                            |
| `template`       | Eine Template-Variable (`var: "csv"`, `var: "body_essentials"`, …)         |
| `firstAttachment`| Den ersten nicht-inline-Anhang mit Endung `.<extension>` (verallgemeinertes `$csv`) |
| `prompt`         | Frage beim Apply (Dialog mit Eingabefeld). Optionales `defaultTemplate` wird vorgeblendet (literal oder `$subject`). |

Die argparse-Schnittstelle muss nicht von Hand erfasst werden: Der
**Analyzer** (siehe unten) liest `parser.add_argument(...)`-Aufrufe aus
einem `.py` und schlägt die Parameterliste samt Typ, Default und
Help-Text vor.

> **`prompt` + Auto-Trigger:** Auto- und Sweeper-Pfade können keinen
> Dialog zeigen und reichen eine leere Wert-Map. Ein `prompt`-Param
> ohne `defaultTemplate` resolvt dann zu leer; ist er `required`,
> läuft der Workflow mit beschreibender Fehlermeldung im Audit-Log auf.
> Wer Auto-Mode mit Prompt-Params will, **muss** ein `defaultTemplate`
> setzen.

---

## Rules

Eine Rule verbindet **Prädikate** (Bedingung), einen **Scope** (wo sie
gilt), einen **Mode** (wie sie auslöst), eine **Action** (was passiert)
und optional **Delay** + **dry-run**.

### Prädikate

Geschlossener Enum, gleiche Motivation wie bei `Step`. Match-Semantik:
**AND** innerhalb einer Rule (alle Prädikate müssen greifen), **OR**
über mehrere Rules eines Workflows (irgendeine feuert).

| Prädikat                 | Match                                                              |
|--------------------------|-------------------------------------------------------------------|
| `fromEmail`              | Volle Absender-Adresse, case-insensitive exakt                    |
| `fromDomain`             | Domain nach `@`, case-insensitive exakt                           |
| `fromDomainIn`           | Domain ist eine aus einer Liste (für „eine Handvoll Vendor-Domains") |
| `subjectContains`        | Substring im Betreff, case-insensitive                            |
| `hasAttachmentExtension` | Mindestens ein nicht-inline-Anhang endet auf `.<ext>` — Compound-Suffixe wie `tar.gz` matchen (eine Rule auf `gz` greift auch bei `tar.gz`) |

Eine **leere** Prädikatliste matcht **nie** (eine Regel, die auf jede
Mail feuert, wäre ein Fußschuss, kein Feature).

### Scope

- `accountId` — `None` = gilt für alle Accounts; `Some` = nur einer
  (z.B. „nur privates Postfach").
- `folderName` — `None` = jeder Ordner; `Some` = nur dieser IMAP-Pfad
  (`INBOX`, `INBOX.Steuer`). **Case-sensitiv** per RFC 3501.

### Mode

| Mode      | Verhalten                                                                  |
|-----------|----------------------------------------------------------------------------|
| `auto`    | Aktion läuft sofort im Hintergrund. Ergebnis ins Audit-Log, kein Popup.    |
| `confirm` | Bestätigungs-Toast unten rechts. User entscheidet (Anwenden / Verwerfen). Sicherer Default für Workflows, die ins Dateisystem schreiben. |

### Action

| Action        | Wirkung                                                                 |
|---------------|-------------------------------------------------------------------------|
| `run_workflow`| Führt den verknüpften Workflow aus. `workflowId` **Pflicht**.           |
| `archive`     | Mail ins Account-Archiv (`archive_folder`). Kein Workflow nötig.        |
| `delete`      | Mail in den Trash (`trash_folder`) — nicht permanent.                   |
| `move`        | Mail nach `actionDest` (Zielordner-Name). Leerer Wert wird beim Save abgewiesen. |

Die drei Direkt-Actions decken die frühere „Lifetime-Rules"-Welt ab —
man muss keinen leeren „Archiv"-Workflow anlegen, um Mails wegzuräumen.

### Delay (`delayMinutes`)

- `0` = sofort fällig. Direkt-Actions feuern beim nächsten Sweep-Tick;
  `run_workflow` + `auto` feuert direkt beim Body-Store.
- `> 0` = `mail.date + delayMinutes` ist das Fälligkeitsdatum. Bis
  dahin trägt die Mail nur ein **Snapshot-Tag** (sichtbar im UI), der
  Sweeper räumt später ab. Minuten als Einheit deckt „in 10 Min weg"
  bis „in 30 Tagen weg" (43200 Min) mit einem Feld ab.

Snapshot-Semantik: Eine getaggte Mail behält ihre ursprüngliche
Action-Absicht, auch wenn die Regel danach geändert oder gelöscht wird.

### Dry-run

`dryRun = true` taggt die Mail (Marker + Hover-Tooltip zeigen, was
*passieren würde*), aber der Sweeper **überspringt** die Ausführung.
UX-Schutz beim ersten Anlegen einer Regel — man sieht den Effekt, ohne
dass Mails verschwinden.

---

## Matching-Pipeline

Rules werden an **zwei** Punkten ausgewertet — bewusst, weil nicht alle
Prädikate zur gleichen Zeit auflösbar sind.

### 1. Sync-Zeit (Fast-Path)

Direkt nach dem Envelope-Upsert im IMAP-Sync
([rule_scheduler.rs](../src-tauri/src/application/rule_scheduler.rs),
`match_at_sync_time`). Hier ist nur der Envelope da, kein Body — also
greifen nur **envelope-auflösbare** Prädikate (`fromEmail`,
`fromDomain`, `fromDomainIn`, `subjectContains`). Die erste passende
Rule liefert ein `ScheduledActionTag`, das nach erfolgreichem Upsert
persistiert wird.

`run_workflow` + `delay = 0` wird hier **übersprungen** — der Workflow
braucht den Body, das übernimmt Stufe 2.

### 2. Body-Store-Zeit (voller Matcher)

Sobald der Body in der DB liegt (Call-Sites in `prefetch::*` und
`body::*`), läuft
[workflow_rules.rs](../src-tauri/src/application/workflow_rules.rs),
`evaluate_and_trigger`. Jetzt sind **alle** Prädikate auflösbar —
inklusive `hasAttachmentExtension`, da die Anhänge aus dem frisch
geschriebenen RFC822 re-geparst werden. Hier wird:

- bei `delay = 0` + `auto` der Workflow direkt im Hintergrund gefeuert;
- bei `delay = 0` + `confirm` ein `workflow-rule-match`-Tauri-Event
  emittiert (→ Toast im Frontend);
- bei `delay > 0` ein `ScheduledActionTag` gesetzt (→ Sweeper).

De-Dupe: Matchen zwei Rules auf denselben Workflow, feuert er nur
einmal. Der Matcher ist **nie fatal** — jeder interne Fehler loggt auf
`warn` und kehrt zurück. Matcher-Bruch darf weder Mail-Sync noch
Body-Download blockieren.

---

## Sweeper

Der Sweeper (`sweep_once`) arbeitet fällige `ScheduledActionTag`s ab.
Er läuft **detached nach jedem Sync** — kein eigener Cron/Timer nötig —
und kann auch manuell angestoßen werden (`run_rule_sweep_now`).

- Lädt bis zu **500** fällige Envelopes (`scheduled_at ≤ now`) pro Lauf.
- Pro Mail: Action ausführen → Audit-Log schreiben.

### Skip-Bedingungen

Eine getaggte Mail wird **übersprungen** (als `skipped` geloggt), wenn:

| Bedingung       | Warum                                                          |
|-----------------|----------------------------------------------------------------|
| `dryRun`        | Regel läuft im Probebetrieb                                     |
| `flagged`       | User hat die Mail markiert — er hat sie offensichtlich gesehen |
| `answered`      | User hat geantwortet — er hat reagiert                          |
| nicht in `INBOX`| User hat sie manuell verschoben — Action wäre redundant         |

So überholt eine Nutzer-Reaktion immer die Automatik: Wer eine
verzögert-zu-löschende Mail flaggt oder beantwortet, behält sie.

---

## Backfill auf Bestand

Beim Anlegen einer Regel kann man sie rückwirkend auf bestehende
INBOX-Mails anwenden (`apply_workflow_rule_to_existing` →
`apply_to_existing`). Das **taggt** bis zu 5000 passende Mails (führt
die Action nicht direkt aus — das macht der Sweeper, der alle
Skip-Bedingungen sauber abbildet). Wer nicht auf den nächsten Sync
warten will, stößt „Sweep jetzt" an.

---

## pi-Training: Regeln aus Beispielen lernen

Statt Prädikate von Hand zu tippen, kann man Beispiel-Mails als
**Training-Candidates** markieren und pi die „engste" passende Regel
vorschlagen lassen
([workflow_training.rs](../src-tauri/src/application/workflow_training.rs)).

Ablauf (`suggest_workflow_rule`):

1. Features der markierten Mails sammeln (Absender/Domain, Subject,
   Folder, Account, Anhänge, Body-Preview).
2. pi-Prompt bauen — instruiert das Modell, den **engsten** Filter zu
   finden, und erklärt Prädikat-Typen, Scope, Mode, Action, Delay,
   dry-run mit Default-Empfehlungen.
3. pi-Oneshot laufen lassen (im Training-Dialog gestreamt, abbrechbar).
4. Antwort parsen → `RuleProposal`. Der Parser ist tolerant
   (snake_case vs. camelCase, `delayDays` → `delayMinutes`) und setzt
   sichere Defaults (`dryRun = true` für Direkt-Actions ohne explizites
   Flag).

Der Vorschlag landet im Dialog mit Predicate-Preview; der User kann
ihn **annehmen**, **bearbeiten** oder **verwerfen**.

---

## Konfiguration

`WorkflowConfig` (persistiert als `workflow_config.json`):

| Feld        | Bedeutung                                                                 |
|-------------|---------------------------------------------------------------------------|
| `scriptDir` | Verzeichnis, aus dem `runScript` Scripts laden darf (Allowlist-Wurzel).   |
| `pythonBin` | Interpreter. Default Windows: `py` (Python-Launcher, überlebt Minor-Bumps); sonst `python3`. Absoluter Pfad überschreibbar. |

### Script-Sicherheit (`runScript`)

Drei Schranken kapseln den einzigen nicht-reversiblen Surface:

1. **Allowlist.** Der *aufgelöste* Script-Pfad muss direktes Kind von
   `scriptDir` sein. Symlink-Tricks, `..`-Traversal und absolute Pfade
   im `script`-Feld werden abgewiesen.
2. **Keine Shell.** argv geht als vor-tokenisierter `Vec<String>` an
   `Command` — kein `sh -c`, keine Shell-Expansion. Ein `$(rm -rf /)`
   im Betreff wird nie zum Befehl.
3. **Template-Vars sind argv, kein Text.** Substitution passiert auf
   ganzen Argument-Tokens. `$csv` wird zu *einem* argv-Eintrag, auch
   wenn der Dateiname Leerzeichen oder Quotes enthält.

---

## Audit-Log

Jede Sweeper-Aktion schreibt einen `RuleActionLogEntry`: Rule-Name,
Action (+ Ziel), Workflow-Id, Message-Id, Subject/Sender-Snapshot,
Ergebnis (`ok`/`skipped`/`failed`) und ggf. Fehlermeldung + Zeitstempel.
Auslesbar über `list_rule_action_log`. Auto-getriggerte Workflows zählen
zusätzlich als Rule-Hit (auch bei partiellem Step-Fehlschlag — die
Regel hat ja korrekt gefeuert, der Fehler liegt am Workflow-Step).

---

## Tauri-Commands (Überblick)

| Bereich        | Commands                                                                 |
|----------------|--------------------------------------------------------------------------|
| Config         | `get_workflow_config`, `set_workflow_config`                             |
| Workflow-CRUD  | `list_workflows`, `add_workflow`, `update_workflow`, `delete_workflow`  |
| Anwenden       | `apply_workflow`                                                         |
| Scripts        | `list_workflow_scripts`, `analyze_python_script`                        |
| Rule-CRUD      | `list_workflow_rules`, `list_workflow_rules_for_workflow`, `add_workflow_rule`, `update_workflow_rule`, `delete_workflow_rule`, `set_workflow_rule_enabled`, `set_workflow_rule_dry_run` |
| Rule anwenden  | `apply_workflow_rule` (Confirm-Pfad), `apply_workflow_rule_to_existing` (Backfill) |
| Sweeper        | `run_rule_sweep_now`, `list_rule_action_log`                            |
| Training       | `list_workflow_training_candidates`, `list_workflow_training_ids`, `is_workflow_training_candidate`, `add_workflow_training`, `remove_workflow_training`, `clear_workflow_training`, `suggest_workflow_rule`, `cancel_workflow_training` |

### Frontend-Berührungspunkte

- [WorkflowSettings.tsx](../src/components/settings/WorkflowSettings.tsx) — Config, Workflow- und Rule-CRUD, Training-Einstieg.
- [WorkflowTrainingDialog.tsx](../src/components/settings/WorkflowTrainingDialog.tsx) — pi-Training (prompt → running → result).
- [WorkflowRuleToast.tsx](../src/components/WorkflowRuleToast.tsx) — Confirm-Mode-Toast (`workflow-rule-match`-Event).
- [WorkflowResultDialog.tsx](../src/components/WorkflowResultDialog.tsx) — Step-für-Step-Ergebnis nach einem Lauf.
- [WorkflowPicker.tsx](../src/components/WorkflowPicker.tsx) / [WorkflowPromptDialog.tsx](../src/components/WorkflowPromptDialog.tsx) — manueller Lauf + Prompt-Param-Eingabe.

---

## Datenmodell

- Domain-Typen: [domain/workflow.rs](../src-tauri/src/domain/workflow.rs)
  (`Workflow`, `Step`, `WorkflowRule`, `RulePredicate`, `RuleAction`,
  `ScheduledActionTag`, `RuleActionLogEntry`).
- Migrations: `0013_workflows.sql` (Basis), `0015_workflow_rules.sql`,
  `0016_workflow_rule_folder.sql`, `0017_workflow_training_candidates.sql`,
  `0025_unify_filters_into_workflow_rules.sql` (Lifetime-Rules in
  Workflow-Rules gefaltet: `name`/`action`/`actionDest`/`dryRun`,
  `workflowId` nullable), `0026_delay_minutes.sql` (`delayDays` →
  `delayMinutes`).

---

## Diagnose

* **Rule feuert nicht auf neue Mail:** Prüfe, ob ein Prädikat den Body
  braucht (`hasAttachmentExtension`) — solche Rules matchen erst zur
  Body-Store-Zeit, nicht beim Sync. Mail einmal öffnen löst den
  Body-Download (und damit den Matcher) aus.
* **Geplante Action passiert nicht:** Sweeper läuft nach jedem Sync —
  manuell „Sweep jetzt" (`run_rule_sweep_now`). Im Audit-Log nachsehen,
  ob die Mail `skipped` wurde (flagged/answered/nicht-INBOX/dry-run).
* **dry-run zeigt Marker, tut aber nichts:** Korrekt — `dryRun` taggt
  nur. Flag in der Rule entfernen, dann greift der Sweeper.
* **Auto-Workflow läuft mit Fehler auf:** Häufig ein `required`
  `prompt`-Param ohne `defaultTemplate` — im Auto/Sweeper-Pfad gibt es
  keinen Dialog. Default setzen oder den Param optional machen.
* **Script wird abgewiesen:** Pfad muss direktes Kind von `scriptDir`
  sein — keine Unterordner, kein `..`, kein absoluter Pfad im
  `script`-Feld.

---

## Roadmap

* **Per-Workflow „confirm before first run"-Gate** — das Modell dafür
  passt schon in `WorkflowRunResult` (siehe Security-Kommentar in
  `workflows.rs`).
* **Subdirectories im Script-Verzeichnis** — aktuell ignoriert der
  Loader Unterordner (Stage 1).
* **Weitere Prädikat-Typen** — z.B. Header-Match, Body-Substring;
  jeder neue Typ braucht bewusst eine Rust-Implementierung (geschlossener
  Enum als Vertrauensgrenze).
