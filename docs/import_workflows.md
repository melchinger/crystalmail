# Import-Workflows: Drafts aus externen Triggern

CrystalMail kann von außen mit einem Markdown-Template + Anhängen
aufgerufen werden und legt daraus einen vorausgefüllten Draft im
Composer ab. Damit lassen sich Versand-Schritte aus Python-Scripten,
Make-Targets, Buchhaltungs-Tools etc. anstoßen — ohne dass die App
ohne Augenkontakt eine Mail rausschickt.

> **Sicherheits-Modell:** Der Trigger erzeugt **immer nur einen Draft**,
> niemals einen automatischen Versand. Der Send-Knopf bleibt manuell.

---

## Aufruf-Form

Zwei Varianten — funktional identisch, eine ist nur skript-freundlicher
beim Argument-Quoting:

### A. Direkt per Kommandozeile

```bat
crystalmail.exe --draft-from-template "C:\templates\rechnung.md" ^
                --param invoice_no=2026-042 ^
                --param customer_email=k@kunde.de ^
                --attach "C:\out\rechnung-2026-042.pdf" ^
                --attach "C:\out\leistungsschein.pdf"
```

Kurzformen: `-T` (Template), `-P` (Param), `-A` (Attach), `-J` (Job-JSON).

### B. Per JSON-Job-Datei

```bat
crystalmail.exe --draft-job "C:\temp\job.json"
```

```json
{
  "template": "C:/templates/rechnung.md",
  "params": {
    "invoice_no": "2026-042",
    "customer_email": "k@kunde.de"
  },
  "attachments": [
    "C:/out/rechnung-2026-042.pdf",
    "C:/out/leistungsschein.pdf"
  ]
}
```

Empfohlen für Skripte: keine Shell-Quoting-Fallen, beliebig viele
Parameter und Anhänge, Pfade mit Leerzeichen/Umlauten unproblematisch.

---

## Verhalten

* **App läuft schon:** Der Aufruf wird vom `single-instance`-Plugin
  abgefangen, der bereits laufende Process verarbeitet den Trigger
  und legt den Draft-Composer geöffnet vor. Hauptfenster kommt nach
  vorn.
* **App läuft nicht:** OS startet die App, der Trigger wird gepuffert
  und sobald die UI hochgefahren ist, springt der Composer auf.

Nur **eine Compose-Instance** zur gleichen Zeit. Mehrere parallele
Trigger sind harmlos, aber der zuletzt gepushte gewinnt — analog zum
sonstigen Compose-Verhalten der App.

---

## Template-Format

Markdown-Datei mit optionalem `key: value`-Frontmatter zwischen `---`-Markern:

```markdown
---
to: $customer_email
cc: backoffice@firma.de
subject: Rechnung $invoice_no — $month $year
account: alice@firma.de
---
Hallo,

anbei die Rechnung $invoice_no für $month $year.
Zahlbar binnen 14 Tagen.

Viele Grüße
Alice
```

### Frontmatter-Felder

| Feld     | Bedeutung                                                    |
|----------|--------------------------------------------------------------|
| `to`     | Empfänger (Pflicht für sinnvollen Draft, sonst leer)         |
| `cc`     | CC-Empfänger (optional)                                       |
| `bcc`    | BCC-Empfänger (optional)                                      |
| `subject`| Betreff (optional, sonst leer)                                |
| `account`| From-Adresse — Composer wählt den passenden Account aus      |

Andere Frontmatter-Keys werden ignoriert. Frontmatter weglassen ist
erlaubt — dann ist der Body alles, alle Header bleiben leer.

### Variablen

Jeder `$name` im Frontmatter UND im Body wird substituiert. Quellen,
in dieser Reihenfolge:

1. **Aufruf-Parameter** (`--param key=value`) — höchste Priorität
2. **Datums-Built-Ins** (siehe Tabelle)

Datums-Built-Ins (Lokal-Zeit, Buchhaltungs-übliche Formate):

| Variable             | Beispiel              |
|----------------------|-----------------------|
| `$date_iso`          | `2026-04-30`          |
| `$date_de`           | `30.04.2026`          |
| `$datetime`          | `2026-04-30 14:32`    |
| `$datetime_seconds`  | `2026-04-30 14:32:05` |
| `$datetime_iso`      | `2026-04-30T14:32`    |
| `$datetime_compact`  | `20260430-1432`       |
| `$time`              | `14:32`               |
| `$time_seconds`      | `14:32:05`            |
| `$year`              | `2026`                |
| `$month`             | `04`                  |
| `$day`               | `30`                  |

Unbekannte Variablen bleiben unverändert stehen (z.B. `$PATH` in einem
Pfad). So sieht ein User-Tippfehler im Composer wie er ist — kein
stiller Datenverlust.

Built-Ins lassen sich bewusst überschreiben: `--param year=2099`
gewinnt gegen das Datum von heute. Für Rechnungs-Templates üblich,
wenn die Buchhaltung gegen ein Vormonats-Datum bucht.

---

## Python-Beispiel

```python
"""
crystalmail_send.py — Rechnungsversand-Helfer.

Findet das fertige Rechnungs-PDF, schreibt eine Job-JSON, ruft
CrystalMail damit auf. Der User sieht den Draft im Composer und
prüft/sendet manuell.
"""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


CRYSTALMAIL = r"C:\Users\Thomas\AppData\Local\CrystalMail\crystalmail.exe"
TEMPLATE = r"C:\Users\Thomas\Documents\CrystalMail\templates\rechnung.md"


def send_invoice(customer_email: str, invoice_no: str, pdf_path: Path,
                 *extra_attachments: Path) -> None:
    job = {
        "template": TEMPLATE,
        "params": {
            "invoice_no": invoice_no,
            "customer_email": customer_email,
        },
        "attachments": [str(pdf_path), *map(str, extra_attachments)],
    }
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".json", delete=False, encoding="utf-8"
    ) as f:
        json.dump(job, f, ensure_ascii=False)
        job_path = f.name

    try:
        subprocess.run(
            [CRYSTALMAIL, "--draft-job", job_path],
            check=True,
        )
    finally:
        # Job-JSON kann sofort weg — die App hat sie synchron beim
        # Start gelesen, danach hat sie keinen Zweck mehr.
        os.unlink(job_path)


if __name__ == "__main__":
    send_invoice(
        customer_email="kunde@example.de",
        invoice_no="2026-042",
        pdf_path=Path(r"C:\out\rechnung-2026-042.pdf"),
    )
```

---

## Diagnose

* **Draft kommt nicht hoch:** Im Tauri-Dev-Modus (`pnpm tauri dev`)
  schaut auf das Terminal — `tracing` loggt
  `draft-import: emitted to live frontend` bzw.
  `draft-import: queued for first frontend mount` auf `info`-Level,
  Parser-Fehler auf `warn`.
* **Variablen nicht aufgelöst:** Im Composer-Body sichtbar als
  rohe `$name` — entweder Tippfehler, oder Param fehlt im Aufruf,
  oder Frontmatter-Markierung war kaputt (siehe nächster Punkt).
* **Frontmatter ignoriert:** Beide `---`-Marker müssen je eine eigene
  Zeile sein, am Datei-Anfang. UTF-8-BOM ist OK. Bei kaputtem
  Frontmatter (kein End-Marker) wird die ganze Datei als Body
  behandelt — Header bleiben leer und der Composer öffnet ohne
  Empfänger.
* **Anhang fehlt:** Pfad-Existenz wird beim Bauen des Drafts geprüft;
  fehlt eine Datei, scheitert der gesamte Import-Auftrag und der
  Composer kommt nicht hoch.

---

## Roadmap

* **Template-Registry**: benannte Templates in den Settings, sodass
  Aufrufer `--template invoice` statt absoluter Pfade nutzen können.
* **URL-Scheme** (`crystalmail://draft?...`): zusätzliche Trigger-
  Form für GUI-Doppelklicks und browser-side Workflows. Auf Windows
  wird das aktuelle Argv-Schema dadurch automatisch mitgenutzt.
* **Markdown-Render**: Body als gerendertes HTML in den Composer
  laden statt Plain-Text-Markdown — derzeit übernimmt der Editor die
  MD-Quelle 1:1 als Plain-Text.
