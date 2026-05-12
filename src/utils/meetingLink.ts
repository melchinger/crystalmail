// Find a "meeting link" inside the free-form `location` field of a
// calendar event. The first http(s) URL on the string wins — anything
// past it is treated as bystander context (room number, dial-in
// fallback, "behind reception", …). Returns `null` if no URL is present.
//
// We deliberately keep the recognition broad rather than Zoom/Teams/Meet
// specific: any HTTP(S) URL that a user types into "Ort" is by intent a
// thing-to-open. Hardcoding allowlists rots — every quarter brings a new
// meeting host.

const URL_RE = /\bhttps?:\/\/[^\s<>"'`)\]}]+/i;

export function detectMeetingUrl(location: string | null | undefined): string | null {
  if (!location) return null;
  const m = location.match(URL_RE);
  if (!m) return null;
  // Strip trailing punctuation that's commonly attached to URLs in prose
  // (period at end of sentence, closing paren when the URL was wrapped).
  let url = m[0];
  while (url.length > 0 && /[.,;:!?]$/.test(url)) {
    url = url.slice(0, -1);
  }
  return url || null;
}
