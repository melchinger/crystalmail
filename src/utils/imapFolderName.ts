/**
 * IMAP folder names are transported in a modified UTF-7 encoding (RFC 3501
 * § 5.1.3). The backend stores the raw server form so we can feed it back
 * to `SELECT` literally — but the UI needs the human-readable Unicode
 * version. Example: `Entw&APw-rfe` → `Entwürfe`.
 *
 * Modified UTF-7 differs from RFC 2152 UTF-7 in two relevant places:
 *   1. Shift character is `&` (not `+`). A literal `&` is encoded as `&-`.
 *   2. The modified base64 alphabet uses `,` instead of `/`.
 *   3. Shifted blocks encode UTF-16BE, not UTF-16LE.
 */
export function decodeImapFolderName(raw: string): string {
  return raw.replace(/&([^-]*)-/g, (_, b64: string) => {
    // `&-` → literal ampersand. The regex captures an empty b64 segment.
    if (b64 === "") return "&";

    // Translate the modified base64 alphabet back to standard (`,` → `/`)
    // and pad to a multiple of 4 so atob() is happy.
    const std = b64.replace(/,/g, "/");
    const padded = std + "===".slice(0, (4 - (std.length % 4)) % 4);

    let bin: string;
    try {
      bin = atob(padded);
    } catch {
      // Malformed sequence — leave it as-is so the user can at least see
      // what's going on rather than losing the name entirely.
      return `&${b64}-`;
    }

    // Each UTF-16BE code unit is two bytes. Hand-assemble surrogate-safe —
    // String.fromCharCode on the raw code units handles surrogate pairs
    // the same way the DOM renders them.
    let out = "";
    for (let i = 0; i + 1 < bin.length; i += 2) {
      const hi = bin.charCodeAt(i);
      const lo = bin.charCodeAt(i + 1);
      out += String.fromCharCode((hi << 8) | lo);
    }
    return out;
  });
}

/**
 * Strip common path noise *after* UTF-7 decoding. Keeps the raw name
 * available in tooltips for debugging.
 *
 *  `INBOX`              → `INBOX`
 *  `[Gmail]/Sent Mail`  → `Sent Mail`
 *  `INBOX.Sent`         → `Sent`
 */
export function displayFolderName(raw: string): string {
  const decoded = decodeImapFolderName(raw);
  if (decoded.toUpperCase() === "INBOX") return "INBOX";
  const stripped = decoded
    .replace(/^\[[^\]]+\]\//, "") // [Gmail]/Sent → Sent
    .replace(/^INBOX[./]/i, ""); // INBOX.Sent → Sent
  return stripped || decoded;
}
