import { useEffect, useImperativeHandle, useRef, forwardRef } from "react";

export type RichEditorHandle = {
  /** Current HTML content — used for the outgoing text/html body. */
  getHtml: () => string;
  /** Flat text extraction — used for the outgoing text/plain body. */
  getText: () => string;
  focus: () => void;
  /**
   * Focus the editor and place the caret at the very start of the content.
   * Used for reply/forward so the user starts typing *above* the signature
   * and quoted block instead of inheriting whatever selection the browser
   * picks by default (which tends to be the end).
   */
  focusStart: () => void;
  /**
   * In-place replace the signature block (`<div class="cm-signature">…`) with
   * a new HTML fragment. Pass `null` to remove the signature entirely.
   * If no signature block is currently present and `html` is non-null, a
   * fresh one is appended at the end of the editor content (above any
   * quoted block, if one exists). Used by Compose when the user changes
   * the From-account: the old signature must vanish, the new one show up,
   * the user's typed body in between stays untouched.
   */
  replaceSignature: (html: string | null) => void;
};

type Props = {
  /** Initial HTML — set once on mount and then owned by the editor (uncontrolled). */
  initialHtml: string;
  placeholder?: string;
  minHeight?: number;
  onChange?: () => void;
  /** Hook invoked when the user pastes an image from the clipboard.
   *  Compose is expected to persist the bytes (temp file via Tauri),
   *  register it as an inline attachment, and return a `contentId` to
   *  reference in `<img src="cid:…">` plus a `previewUrl` (blob:) for
   *  immediate in-editor display. If absent or returning `null`, the
   *  image paste is silently dropped and we fall back to text-paste
   *  (which yields nothing for an image-only clipboard). */
  onPasteImage?: (file: File) => Promise<{
    contentId: string;
    previewUrl: string;
  } | null>;
};

/**
 * Small rich text editor based on contentEditable + document.execCommand.
 * execCommand is deprecated but still shipped in every engine we care about,
 * and trading it for ProseMirror/TipTap would more than double the bundle.
 * If we ever need tables or collaborative editing we can swap it then.
 */
export const RichEditor = forwardRef<RichEditorHandle, Props>(function RichEditor(
  { initialHtml, placeholder, minHeight = 220, onChange, onPasteImage },
  ref,
) {
  const rootRef = useRef<HTMLDivElement>(null);

  // Seed innerHTML exactly once; after that, the DOM is the source of truth
  // and re-seeding would wipe the user's caret and edits.
  useEffect(() => {
    if (rootRef.current && rootRef.current.innerHTML === "") {
      rootRef.current.innerHTML = initialHtml;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useImperativeHandle(ref, () => ({
    getHtml: () => rootRef.current?.innerHTML ?? "",
    getText: () => rootRef.current?.innerText ?? "",
    focus: () => rootRef.current?.focus(),
    focusStart: () => {
      const el = rootRef.current;
      if (!el) return;
      el.focus();
      const sel = window.getSelection();
      if (!sel) return;
      const range = document.createRange();
      range.selectNodeContents(el);
      range.collapse(true); // collapse to start — caret lands at first text position
      sel.removeAllRanges();
      sel.addRange(range);
    },
    replaceSignature: (html: string | null) => {
      const root = rootRef.current;
      if (!root) return;
      const existing = root.querySelector<HTMLElement>(".cm-signature");
      if (html && html.trim().length > 0) {
        if (existing) {
          // In-place swap. Caller passes already-sanitized HTML (we
          // don't sanitize here to keep this hook stupid).
          existing.innerHTML = html;
        } else {
          // Insert fresh signature block. Best location: right before
          // any quoted block (a `<blockquote>` or sibling we don't
          // recognize), otherwise at the very end. Keep one empty
          // line as separator so the visual rhythm matches what
          // Compose builds at mount time.
          const sig = document.createElement("div");
          sig.className = "cm-signature";
          sig.innerHTML = html;
          const spacer = document.createElement("div");
          spacer.innerHTML = "<br>";
          const quote = root.querySelector("blockquote");
          if (quote && quote.parentElement === root) {
            root.insertBefore(spacer, quote);
            root.insertBefore(sig, quote);
          } else {
            root.appendChild(spacer);
            root.appendChild(sig);
          }
        }
      } else if (existing) {
        // Remove signature plus the spacer-line directly above it
        // (the `<div><br></div>` we add when building the initial
        // HTML in Compose). Without this the user would be left with
        // an empty paragraph hanging at the bottom of their body.
        const prev = existing.previousElementSibling;
        if (
          prev &&
          prev.tagName === "DIV" &&
          prev.textContent === "" &&
          prev.querySelector("br")
        ) {
          prev.remove();
        }
        existing.remove();
      }
    },
  }));

  return (
    <div className="flex flex-col">
      <Toolbar editorRef={rootRef} onChange={onChange} />
      <div
        ref={rootRef}
        contentEditable
        suppressContentEditableWarning
        data-placeholder={placeholder}
        onInput={onChange}
        onPaste={(e) => {
          // Image paste takes precedence over text. Outlook/Word also put
          // a text/plain fallback ("Inline-image") alongside the image
          // when copying out of a mail; without this check we'd never
          // reach the image branch.
          if (onPasteImage) {
            const items = e.clipboardData.items;
            let imageFile: File | null = null;
            for (let i = 0; i < items.length; i++) {
              const it = items[i];
              if (it.kind === "file" && it.type.startsWith("image/")) {
                imageFile = it.getAsFile();
                break;
              }
            }
            if (imageFile) {
              e.preventDefault();
              // Snapshot the editor element synchronously — by the time
              // the await resolves React may have re-rendered or the user
              // may have clicked elsewhere; we still want the insert to
              // land in this editor instance.
              const editor = rootRef.current;
              onPasteImage(imageFile).then((result) => {
                if (!result || !editor) return;
                // Restore caret to the editor (the await may have stolen
                // focus, depending on Tauri dialog usage downstream).
                editor.focus();
                const safeCid = result.contentId.replace(/[^a-zA-Z0-9._\-@]/g, "");
                const safeUrl = result.previewUrl.replace(/"/g, "&quot;");
                // `data-cid` survives the round-trip through the editor's
                // innerHTML — Send-time rewriting in `buildSendRequest`
                // swaps `src=blob:…` for `src=cid:CID` using exactly this
                // attribute as the lookup key.
                const html = `<img src="${safeUrl}" data-cid="${safeCid}" alt="" style="max-width: 100%;">`;
                document.execCommand("insertHTML", false, html);
                // execCommand sometimes skips the `input` event when the
                // insertion happens after an async gap; nudge onChange so
                // the dirty-flag in Compose flips.
                onChange?.();
              });
              return;
            }
          }
          // Paste as plain text by default so we don't import Word/Outlook
          // styles that clash with our inline styles. Users can still use
          // Ctrl+V for rich paste by holding Shift (browser default) — no,
          // that's the opposite. For MVP plain-paste is the sane default.
          e.preventDefault();
          const text = e.clipboardData.getData("text/plain");
          document.execCommand("insertText", false, text);
        }}
        className="cm-mail-canvas resize-vertical overflow-y-auto rounded-md px-3 py-2 text-sm"
        style={{
          minHeight,
          background: "var(--bg-base)",
          // Eigene High-Contrast-Textfarbe, NICHT `var(--fg-base)`.
          // Im Dark-Mode liefert das CSS-Token oklch(0.96 …) → ein
          // off-white das auf ClearType-Subpixel-Rendering subjektiv
          // matt wirkt. Die `cm-mail-canvas`-Klasse setzt unten in
          // einer @media-Regel pure white für dark mode. Light Mode
          // braucht den Override nicht.
          color: "var(--fg-base)",
          border: "1px solid var(--border-base)",
          outline: "none",
        }}
      />
      <style>{`
        .cm-mail-canvas[data-placeholder]:empty::before {
          content: attr(data-placeholder);
          color: var(--fg-subtle);
          pointer-events: none;
        }
        .cm-mail-canvas blockquote {
          border-left: 3px solid #d1d5db;
          margin: 0.5em 0;
          padding: 0.25em 0.75em;
          color: #6b7280;
        }
        .cm-mail-canvas a { color: #2563eb; }
        .cm-mail-canvas img { max-width: 100%; height: auto; }
        .cm-mail-canvas table { max-width: 100%; }
        .cm-mail-canvas pre, .cm-mail-canvas code {
          font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
          font-size: 13px;
        }
        @media (prefers-color-scheme: dark) {
          /* Dark-Mode-Mail-Body bekommt pure white Text + dezenten
             Bold-Bump. Die App-globale --fg-base liegt bei oklch(0.96 …)
             → off-white. Auf großen Mail-Body-Flächen wirkt das je
             nach Display und Font-Smoothing matt; pure white liefert
             da deutlich mehr Lesbarkeit, ohne in der Liste/Sidebar
             zu schreien (die behalten die soft-token-Variante). */
          .cm-mail-canvas {
            color: #ffffff !important;
          }
          .cm-mail-canvas blockquote {
            border-left-color: #4b5563;
            color: #d1d5db;
          }
          .cm-mail-canvas a { color: #93c5fd; }
        }
      `}</style>
    </div>
  );
});

function Toolbar({
  editorRef,
  onChange,
}: {
  editorRef: React.RefObject<HTMLDivElement>;
  onChange?: () => void;
}) {
  const cmd = (command: string, value?: string) => {
    editorRef.current?.focus();
    document.execCommand(command, false, value);
    onChange?.();
  };

  const insertLink = () => {
    const url = window.prompt("URL:", "https://");
    if (!url) return;
    cmd("createLink", url);
  };

  return (
    <div
      className="mb-1 flex flex-wrap items-center gap-1 rounded-md border px-1.5 py-1 text-xs"
      style={{
        borderColor: "var(--border-base)",
        background: "var(--bg-panel)",
      }}
    >
      <Btn onClick={() => cmd("bold")} title="Fett (Ctrl+B)">
        <b>B</b>
      </Btn>
      <Btn onClick={() => cmd("italic")} title="Kursiv (Ctrl+I)">
        <i>I</i>
      </Btn>
      <Btn onClick={() => cmd("underline")} title="Unterstrichen (Ctrl+U)">
        <u>U</u>
      </Btn>
      <Divider />
      <Btn onClick={() => cmd("insertUnorderedList")} title="Liste">
        •
      </Btn>
      <Btn onClick={() => cmd("insertOrderedList")} title="Nummerierte Liste">
        1.
      </Btn>
      <Btn onClick={() => cmd("formatBlock", "blockquote")} title="Zitat">
        ❝
      </Btn>
      <Divider />
      <Btn onClick={insertLink} title="Link">
        🔗
      </Btn>
      <Btn onClick={() => cmd("removeFormat")} title="Formatierung entfernen">
        ⌫
      </Btn>
    </div>
  );
}

function Btn({
  onClick,
  title,
  children,
}: {
  onClick: () => void;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      // tabIndex=-1: the formatting toolbar shouldn't steal a tab-stop
      // between the Subject input and the editor body. Mouse access is
      // unaffected; users who want keyboard-only formatting still have
      // Ctrl+B / Ctrl+I / Ctrl+U via the contentEditable defaults.
      tabIndex={-1}
      onMouseDown={(e) => {
        // Prevent focus stealing so the selection in the editor stays intact.
        e.preventDefault();
        onClick();
      }}
      title={title}
      className="rounded px-2 py-0.5 transition-colors"
      style={{ color: "var(--fg-base)" }}
      onMouseEnter={(e) => (e.currentTarget.style.background = "var(--bg-hover)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      {children}
    </button>
  );
}

function Divider() {
  return (
    <span
      className="mx-0.5 inline-block h-4 w-px"
      style={{ background: "var(--border-base)" }}
      aria-hidden
    />
  );
}
