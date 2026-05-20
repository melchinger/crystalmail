import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AddressCompletion } from "../types";

type Props = {
  /** Anchor-Element (das To/Cc/Bcc-Input). Wir messen seine BoundingBox
   *  und positionieren das Dropdown direkt drunter, damit es bei
   *  Resize/Scroll mitwandert ohne Layout-Pflege im Parent. */
  anchorRef: React.RefObject<HTMLInputElement>;
  /** Aktueller Wert des Inputs — bei `mode="compose"` suchen wir nach
   *  dem Token rechts vom letzten Komma (Mehrfach-Empfänger-Eingabe wie
   *  "alice@x.de, bob"); bei `mode="single"` ist `value` selbst der
   *  Prefix. */
  value: string;
  /** Bei `mode="compose"` aufgerufen wenn der User eine Auswahl
   *  bestätigt: ersetzt den letzten Token durch das gewählte Item.
   *  Caller setzt damit den Input-State. Optional, wenn
   *  `onPickContact` reicht. */
  onPick?: (formatted: string) => void;
  /** Alternativ-Callback: liefert das komplette Completion-Objekt
   *  zurück, damit der Caller frei entscheiden kann was passieren soll
   *  (Email-Feld füllen, Name-Feld füllen, Liste erweitern). Hat
   *  Vorrang vor `onPick` wenn beide gesetzt sind. Pflicht bei
   *  `mode="single"`. */
  onPickContact?: (c: AddressCompletion) => void;
  /** Optional: open/close-Steuerung via Parent (Esc, Click-outside). */
  onClose?: () => void;
  /** `"compose"` (default): Komma-Liste, der letzte Token ist der
   *  Suchprefix, Pick fügt den gewählten Adressblock am Token-Ende
   *  ein. `"single"`: ein Input = ein Empfänger, Pick ersetzt komplett
   *  und der Caller entscheidet via `onPickContact` was passiert. */
  mode?: "compose" | "single";
};

/** Mindest-Prefix-Länge bevor wir das Backend belasten. 1 Zeichen würde
 *  bei großen Adressbüchern oft 100+ Hits liefern, 2 reichen für
 *  sinnvolles Narrowing. */
const MIN_PREFIX = 2;
const DEBOUNCE_MS = 120;

/** Extrahiere den letzten unvollständigen Empfänger aus einer Komma-
 *  getrennten Liste. "alice@x.de, bo" → "bo". Bei "" → "". */
function lastToken(value: string): { prefix: string; head: string } {
  const lastComma = value.lastIndexOf(",");
  if (lastComma < 0) return { prefix: value.trim(), head: "" };
  const head = value.slice(0, lastComma + 1);
  const prefix = value.slice(lastComma + 1).trim();
  return { prefix, head: head + " " };
}

/** "Alice <alice@x.de>" oder "alice@x.de" — der Industrie-Standard fürs
 *  Adress-Feld. Der RFC-Parser auf Backend-Seite (lettre Mailbox::parse)
 *  versteht beides. Mit Display-Name nur wenn vorhanden und nicht
 *  identisch zum Local-Part. */
function formatAddress(c: AddressCompletion): string {
  const name = c.contactDisplayName ?? c.displayName ?? "";
  if (!name.trim()) return c.email;
  // Wenn der Name lediglich der Local-Part der E-Mail ist, weglassen
  // (kein Mehrwert).
  const localPart = c.email.split("@")[0]?.toLowerCase() ?? "";
  if (name.trim().toLowerCase() === localPart) return c.email;
  // Anführungszeichen wenn der Name Sonderzeichen hat — minimaler
  // RFC-2822-Korrektheit-Aufwand. Backslash MUSS vor dem Anführungs-
  // zeichen escaped werden, sonst macht der zweite Replace aus einem
  // bereits-escapten `\"` ein doppelt-escaptes `\\"` und der Quoted-
  // String beim Empfänger ist kaputt.
  const needsQuote = /[",;<>@]/.test(name);
  const escaped = name.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  const safe = needsQuote ? `"${escaped}"` : name;
  return `${safe} <${c.email}>`;
}

export function AddressAutocomplete({
  anchorRef,
  value,
  onPick,
  onPickContact,
  onClose,
  mode = "compose",
}: Props) {
  const [items, setItems] = useState<AddressCompletion[]>([]);
  const [highlight, setHighlight] = useState(0);
  const [box, setBox] = useState<{ left: number; top: number; width: number } | null>(
    null,
  );
  const debounceRef = useRef<number | null>(null);
  const lastQueryRef = useRef<string>("");

  // Single-mode: whole input is the prefix, no comma-token slicing,
  // no "head" prefix to splice in front of the picked address.
  const { prefix, head } =
    mode === "single"
      ? { prefix: value.trim(), head: "" }
      : lastToken(value);

  // Shared selection dispatcher. `onPickContact` is the semantic-level
  // hook (gets the full object); `onPick` is the legacy
  // formatted-string hook used by Compose. We prefer Contact when both
  // are wired so a future call-site can opt into the richer API
  // without removing onPick for callers that still rely on it.
  const dispatchPick = (c: AddressCompletion) => {
    if (onPickContact) {
      onPickContact(c);
      return;
    }
    if (onPick) {
      const formatted = formatAddress(c);
      onPick(
        mode === "single" ? formatted : `${head}${formatted}, `,
      );
    }
  };

  // Debounced Backend-Lookup. Cancel pending lookups wenn der User
  // weitertippt — sonst kommt die alte Antwort zurück und überschreibt
  // die aktuelle (race condition).
  useEffect(() => {
    if (debounceRef.current !== null) {
      window.clearTimeout(debounceRef.current);
      debounceRef.current = null;
    }
    if (prefix.length < MIN_PREFIX) {
      setItems([]);
      return;
    }
    const myQuery = prefix.toLowerCase();
    lastQueryRef.current = myQuery;
    debounceRef.current = window.setTimeout(() => {
      void (async () => {
        try {
          const result = await invoke<AddressCompletion[]>(
            "list_address_completions",
            { prefix: myQuery, limit: 8 },
          );
          // Race-Schutz: wenn der User in der Zwischenzeit weitergetippt
          // hat, ist `lastQueryRef` schon wieder ein anderer Wert →
          // ignorieren.
          if (lastQueryRef.current === myQuery) {
            setItems(result);
            setHighlight(0);
          }
        } catch (e) {
          console.warn("address completion failed:", e);
          setItems([]);
        }
      })();
    }, DEBOUNCE_MS);
    return () => {
      if (debounceRef.current !== null) {
        window.clearTimeout(debounceRef.current);
        debounceRef.current = null;
      }
    };
  }, [prefix]);

  // Position-Tracking. Bei jedem Render lesen wir die Box des Anchors
  // — nicht teuer und vermeidet Listener-Komplexität für scroll/resize.
  useEffect(() => {
    const el = anchorRef.current;
    if (!el || items.length === 0) {
      setBox(null);
      return;
    }
    const r = el.getBoundingClientRect();
    setBox({ left: r.left, top: r.bottom + 2, width: r.width });
  }, [anchorRef, items, value]);

  // Keyboard-Handler auf Window-Ebene. Wir wollen die Pfeiltasten +
  // Enter/Tab direkt aus dem Input abgreifen, ohne dass Compose seinen
  // eigenen Tab-Handler bricht. Strategie: Listener am Window mit
  // Capture-Phase, früh aussteigen wenn das Dropdown nicht aktiv ist.
  useEffect(() => {
    if (items.length === 0) return;
    const onKey = (e: KeyboardEvent) => {
      // Nur reagieren wenn der Anchor der aktuelle Focus ist — sonst
      // hätten wir z.B. im Subject-Input mit Pfeil-runter trotzdem
      // hier was abgegriffen.
      if (document.activeElement !== anchorRef.current) return;
      switch (e.key) {
        case "ArrowDown":
          e.preventDefault();
          setHighlight((h) => (h + 1) % items.length);
          break;
        case "ArrowUp":
          e.preventDefault();
          setHighlight((h) => (h - 1 + items.length) % items.length);
          break;
        case "Enter":
        case "Tab":
          e.preventDefault();
          {
            const picked = items[highlight];
            if (picked) {
              dispatchPick(picked);
            }
          }
          setItems([]);
          break;
        case "Escape":
          e.preventDefault();
          setItems([]);
          onClose?.();
          break;
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [items, highlight, onPick, onPickContact, onClose, anchorRef, mode]);

  if (items.length === 0 || !box) return null;

  return (
    <ul
      className="fixed z-50 max-h-[280px] overflow-y-auto rounded-md border shadow-lg"
      style={{
        left: box.left,
        top: box.top,
        width: box.width,
        background: "var(--bg-panel)",
        borderColor: "var(--border-base)",
        color: "var(--fg-base)",
      }}
      role="listbox"
    >
      {items.map((c, i) => {
        const active = i === highlight;
        const display = c.contactDisplayName ?? c.displayName ?? "";
        return (
          <li
            key={`${c.email}-${i}`}
            role="option"
            aria-selected={active}
            className="cursor-pointer border-b px-3 py-1.5 text-sm last:border-b-0"
            style={{
              background: active ? "var(--bg-hover)" : "transparent",
              borderColor: "var(--border-soft)",
            }}
            onMouseEnter={() => setHighlight(i)}
            // mousedown statt click: input verliert sonst Focus bevor
            // unser Pick durchläuft — preventDefault hält den Focus
            // im Input.
            onMouseDown={(e) => {
              e.preventDefault();
              dispatchPick(c);
              setItems([]);
            }}
          >
            <div className="flex items-baseline justify-between gap-3">
              <div className="min-w-0 flex-1 truncate">
                {display && (
                  <span className="font-medium" style={{ color: "var(--fg-base)" }}>
                    {display}
                  </span>
                )}
                {display && (
                  <span className="ml-2" style={{ color: "var(--fg-subtle)" }}>
                    {c.email}
                  </span>
                )}
                {!display && <span>{c.email}</span>}
              </div>
              {c.contactId && (
                <span
                  className="shrink-0 rounded px-1 text-[10px]"
                  title="Aus Adressbuch"
                  style={{
                    background: "var(--bg-hover)",
                    color: "var(--fg-muted)",
                  }}
                >
                  ✓
                </span>
              )}
            </div>
          </li>
        );
      })}
    </ul>
  );
}
