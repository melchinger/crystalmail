/**
 * Trusted-sender allowlist for the remote-image gate.
 *
 * Default behaviour: external `<img src=https://…>` references in mail
 * HTML are CSP-blocked; the user can opt in per-message via the "Bilder
 * laden" banner. This module backs two persistent opt-ins:
 *
 *   * **Per-address** ("vom Absender immer laden") — the exact From:
 *     address goes into the allowlist. Future mails from that address
 *     load images automatically.
 *
 *   * **Per-domain** ("gesamte Domain immer laden") — only the domain
 *     part is stored. Useful for newsletters whose local-part rotates
 *     (`hash@news.stripe.com`) but whose domain is stable. A domain
 *     match wins regardless of the local-part.
 *
 * Storage: localStorage. This is a pure UI/privacy preference scoped
 * to the user's machine — no per-account distinction (you trust
 * `@stripe.com` regardless of which CrystalMail account the mail
 * landed in). JSON shape is `{ addresses: string[], domains: string[] }`.
 *
 * Legacy migration: an earlier version stored a flat `string[]` of
 * addresses. We detect that shape on read and lift it into
 * `{ addresses, domains: [] }` transparently. Next write commits the
 * new shape, so the migration is one-way and self-healing.
 */

const STORAGE_KEY = "crystalmail:trustedSenders";

/**
 * DOM event broadcast on every change, so multiple Readers + the
 * settings panel stay in sync without a shared store. Plain
 * CustomEvent — no payload needed; consumers re-read on every fire.
 */
export const TRUSTED_SENDERS_CHANGED = "cm:trusted-senders-changed";

/** Structured snapshot. Returned by `loadTrustedSenders()`. */
export type TrustedSenders = {
  addresses: Set<string>;
  domains: Set<string>;
};

/** Lowercase + trim. Empty/invalid → null so callers can short-circuit. */
function normalize(s: string | null | undefined): string | null {
  if (!s) return null;
  const t = s.trim().toLowerCase();
  return t.length === 0 ? null : t;
}

/**
 * Extract the domain part of an email address. Returns `null` for
 * malformed input (no `@`, empty domain). The domain is normalised
 * to lowercase but otherwise kept verbatim — IDN punycode etc. is
 * the caller's job, we don't attempt UTS46.
 */
export function extractDomain(email: string | null | undefined): string | null {
  const n = normalize(email);
  if (!n) return null;
  const at = n.lastIndexOf("@");
  if (at < 1 || at === n.length - 1) return null;
  return n.slice(at + 1);
}

function readRaw(): TrustedSenders {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { addresses: new Set(), domains: new Set() };
    const parsed = JSON.parse(raw) as unknown;

    // Legacy shape: flat string[] of addresses. Lift into the new
    // structure; the next write will commit the new shape.
    if (Array.isArray(parsed)) {
      const addresses = new Set<string>();
      for (const v of parsed) {
        const n = typeof v === "string" ? normalize(v) : null;
        if (n) addresses.add(n);
      }
      return { addresses, domains: new Set() };
    }

    if (parsed && typeof parsed === "object") {
      const obj = parsed as { addresses?: unknown; domains?: unknown };
      const addresses = new Set<string>();
      const domains = new Set<string>();
      if (Array.isArray(obj.addresses)) {
        for (const v of obj.addresses) {
          const n = typeof v === "string" ? normalize(v) : null;
          if (n) addresses.add(n);
        }
      }
      if (Array.isArray(obj.domains)) {
        for (const v of obj.domains) {
          const n = typeof v === "string" ? normalize(v) : null;
          if (n) domains.add(n);
        }
      }
      return { addresses, domains };
    }

    return { addresses: new Set(), domains: new Set() };
  } catch {
    return { addresses: new Set(), domains: new Set() };
  }
}

function writeRaw(state: TrustedSenders): void {
  try {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        addresses: [...state.addresses].sort(),
        domains: [...state.domains].sort(),
      }),
    );
  } catch {
    // localStorage may be disabled / quota'd — degrade to session-only
    // behaviour rather than blowing up the Reader.
  }
}

function broadcast(): void {
  window.dispatchEvent(new CustomEvent(TRUSTED_SENDERS_CHANGED));
}

/** Snapshot of the full allowlist. Fresh sets per call. */
export function loadTrustedSenders(): TrustedSenders {
  return readRaw();
}

/**
 * True when this email is trusted — either by exact address or by its
 * domain being on the domain allowlist. Domain match wins implicitly
 * because both branches return true.
 */
export function isTrustedSender(email: string | null | undefined): boolean {
  const n = normalize(email);
  if (!n) return false;
  const state = readRaw();
  if (state.addresses.has(n)) return true;
  const dom = extractDomain(n);
  if (dom && state.domains.has(dom)) return true;
  return false;
}

/**
 * Detail on *why* a sender is trusted — used by the Reader banner to
 * show the right message ("Domain ist freigegeben" vs "Adresse ist
 * freigegeben"). Returns `null` when the sender isn't trusted.
 */
export type TrustReason =
  | { kind: "address"; address: string }
  | { kind: "domain"; domain: string };

export function trustReasonFor(
  email: string | null | undefined,
): TrustReason | null {
  const n = normalize(email);
  if (!n) return null;
  const state = readRaw();
  if (state.addresses.has(n)) return { kind: "address", address: n };
  const dom = extractDomain(n);
  if (dom && state.domains.has(dom)) return { kind: "domain", domain: dom };
  return null;
}

/** Add an exact address. No-op when already present. Broadcasts on change. */
export function addTrustedSender(email: string): void {
  const n = normalize(email);
  if (!n) return;
  const state = readRaw();
  if (state.addresses.has(n)) return;
  state.addresses.add(n);
  writeRaw(state);
  broadcast();
}

/** Remove an exact address. */
export function removeTrustedSender(email: string): void {
  const n = normalize(email);
  if (!n) return;
  const state = readRaw();
  if (state.addresses.delete(n)) {
    writeRaw(state);
    broadcast();
  }
}

/**
 * Add a domain. Accepts either a bare domain (`stripe.com`) or a full
 * email — in the latter case the domain part is extracted. No-op
 * when already present.
 */
export function addTrustedDomain(domainOrEmail: string): void {
  const n = normalize(domainOrEmail);
  if (!n) return;
  const dom = n.includes("@") ? extractDomain(n) : n;
  if (!dom) return;
  const state = readRaw();
  if (state.domains.has(dom)) return;
  state.domains.add(dom);
  writeRaw(state);
  broadcast();
}

/** Remove a domain. */
export function removeTrustedDomain(domain: string): void {
  const n = normalize(domain);
  if (!n) return;
  const state = readRaw();
  if (state.domains.delete(n)) {
    writeRaw(state);
    broadcast();
  }
}
