/**
 * HTML sanitization + extraction helpers for mail bodies and reply/forward
 * quoting.
 *
 * Two common shapes come in:
 *   1. `mail-parser` hands us the full original document: `<html><head>…</head><body>…</body></html>`
 *   2. Some senders inline everything as a body fragment.
 * We need one cleaned body fragment either way, safe to embed in our
 * contentEditable editor and in the outgoing `text/html` MIME part.
 */

/**
 * Return the inner body of an HTML document. If the input is already a
 * fragment (no `<body>` tag), it is returned unchanged. Robust against
 * missing `<html>`/`<head>` — DOMParser always fabricates those implicitly.
 */
export function extractHtmlBody(raw: string): string {
  if (!raw) return "";
  try {
    const doc = new DOMParser().parseFromString(raw, "text/html");
    return doc.body ? doc.body.innerHTML : raw;
  } catch {
    return raw;
  }
}

/**
 * Strip everything we never want to execute or side-effect — scripts, event
 * handlers, framed content, javascript: URLs. Deliberately keeps inline
 * `<style>` blocks so the quoted original retains its look, and keeps
 * `cid:` image refs so they render while being composed (they won't make
 * it across the network, but they're harmless once the recipient sees them
 * since no outbound fetch happens).
 */
export function sanitizeFragment(fragment: string): string {
  if (!fragment) return "";
  let doc: Document;
  try {
    doc = new DOMParser().parseFromString(
      `<!DOCTYPE html><body>${fragment}</body>`,
      "text/html",
    );
  } catch {
    return "";
  }

  doc
    .querySelectorAll("script, iframe, object, embed, meta, link, base")
    .forEach((n) => n.remove());

  doc.querySelectorAll("*").forEach((el) => {
    [...el.attributes].forEach((attr) => {
      const name = attr.name.toLowerCase();
      const value = attr.value;
      if (name.startsWith("on")) el.removeAttribute(attr.name);
      if (
        (name === "href" || name === "src" || name === "xlink:href") &&
        /^\s*javascript:/i.test(value)
      ) {
        el.removeAttribute(attr.name);
      }
    });
  });

  return doc.body?.innerHTML ?? "";
}

/**
 * Compose step: take a (possibly full-document) HTML blob from the original
 * message and produce a fragment safe to insert into our contentEditable.
 * Combines body extraction with sanitization.
 */
export function prepareQuoteForEditor(html: string): string {
  return sanitizeFragment(extractHtmlBody(html));
}

/**
 * Send-time rewrite: replace the `src=` of every `<img data-cid="X" …>`
 * in `html` with `src="cid:X"`. Used by the compose path because the
 * pasted-image flow displays the image via a `blob:` URL in the editor
 * (so the user sees an immediate preview) but the outgoing MIME has to
 * reference the inline attachment by its Content-ID instead. The
 * `data-cid` attribute is then dropped — it's an internal marker, not
 * something the recipient should see.
 *
 * Tolerant of arbitrary attribute order and quote style: works for
 * `<img data-cid="X" src="blob:…" …>` and `<img src='blob:…' data-cid='X'>`
 * alike. Images without a `data-cid` are left alone — they're pasted
 * `<img src="https://…">` references the user typed in by hand.
 */
export function rewriteInlineImageSrcs(html: string): string {
  if (!html.includes("data-cid")) return html;
  let doc: Document;
  try {
    doc = new DOMParser().parseFromString(
      `<!DOCTYPE html><body>${html}</body>`,
      "text/html",
    );
  } catch {
    return html;
  }
  doc.querySelectorAll("img[data-cid]").forEach((img) => {
    const cid = img.getAttribute("data-cid");
    if (!cid) return;
    img.setAttribute("src", `cid:${cid}`);
    img.removeAttribute("data-cid");
  });
  return doc.body?.innerHTML ?? html;
}

export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

export function plainToHtml(s: string): string {
  return escapeHtml(s).replace(/\r?\n/g, "<br>");
}

export function stripHtmlToText(html: string): string {
  return html
    .replace(/<style[\s\S]*?<\/style>/gi, "")
    .replace(/<script[\s\S]*?<\/script>/gi, "")
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<\/p>/gi, "\n\n")
    .replace(/<[^>]+>/g, "")
    .replace(/&nbsp;/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

/**
 * Wrap the editor's fragment in a minimal HTML document so strict MUAs
 * (notably Outlook) render it as rich text instead of degrading to plain.
 */
export function wrapAsHtmlDocument(fragment: string): string {
  return (
    `<!DOCTYPE html>` +
    `<html>` +
    `<head><meta charset="utf-8"></head>` +
    `<body style="font-family:-apple-system,BlinkMacSystemFont,Segoe UI,Roboto,sans-serif;font-size:14px;line-height:1.5;color:#1f2937;">` +
    fragment +
    `</body>` +
    `</html>`
  );
}
