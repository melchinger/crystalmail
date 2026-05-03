import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  TRUSTED_SENDERS_CHANGED,
  addTrustedDomain,
  addTrustedSender,
  loadTrustedSenders,
  removeTrustedDomain,
  removeTrustedSender,
} from "../../utils/trustedSenders";

/**
 * Trusted-sender management panel. Shows the two allowlists the Reader
 * uses to skip its remote-image gate: exact addresses (`alice@example.com`)
 * and whole domains (`stripe.com`). Each row gets an X to remove it.
 *
 * The Reader writes the same allowlist via its banner checkboxes; this
 * panel exists for two cases the Reader can't cover:
 *
 *   1. Untrust someone who isn't actively writing to you (no mail in the
 *      list to open the banner from).
 *   2. Add an entry you know about up front, before any mail arrives.
 *
 * Live sync: every mutation broadcasts `cm:trusted-senders-changed`,
 * and this component listens to the same event so removes from one
 * Reader instance update the panel without a manual reload.
 */
export function TrustedSendersSettings() {
  const { t } = useTranslation();
  const [snapshot, setSnapshot] = useState(() => loadTrustedSenders());
  const [newAddress, setNewAddress] = useState("");
  const [newDomain, setNewDomain] = useState("");
  // Separate error slots so a typo in one input doesn't blank the
  // other — keeps the UX gentler.
  const [addressError, setAddressError] = useState<string | null>(null);
  const [domainError, setDomainError] = useState<string | null>(null);

  useEffect(() => {
    const onChange = () => setSnapshot(loadTrustedSenders());
    window.addEventListener(TRUSTED_SENDERS_CHANGED, onChange);
    return () => {
      window.removeEventListener(TRUSTED_SENDERS_CHANGED, onChange);
    };
  }, []);

  const sortedAddresses = useMemo(
    () => [...snapshot.addresses].sort(),
    [snapshot],
  );
  const sortedDomains = useMemo(
    () => [...snapshot.domains].sort(),
    [snapshot],
  );

  const submitAddress = () => {
    const v = newAddress.trim();
    if (!v) return;
    if (!v.includes("@") || v.indexOf("@") === v.length - 1) {
      setAddressError(t("trustedSenders.addressInvalid"));
      return;
    }
    addTrustedSender(v);
    setNewAddress("");
    setAddressError(null);
  };

  const submitDomain = () => {
    const v = newDomain.trim();
    if (!v) return;
    // Accept either a bare domain or a full email (the util extracts
    // the domain part either way). Reject obvious nonsense — a
    // domain with no dot is almost always a typo (`localhost` aside,
    // which isn't relevant for mail).
    if (!v.includes(".")) {
      setDomainError(t("trustedSenders.domainInvalid"));
      return;
    }
    addTrustedDomain(v);
    setNewDomain("");
    setDomainError(null);
  };

  return (
    <div className="flex flex-col gap-5">
      <header>
        <h2 className="text-base font-semibold">
          {t("trustedSenders.title")}
        </h2>
        <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("trustedSenders.hint")}
        </p>
      </header>

      {/* Address allowlist */}
      <section className="flex flex-col gap-2">
        <h3 className="text-sm font-medium">
          {t("trustedSenders.addressesHeading")}
        </h3>
        <p className="text-[11px]" style={{ color: "var(--fg-muted)" }}>
          {t("trustedSenders.addressesHint")}
        </p>
        <div className="flex items-center gap-2">
          <input
            type="email"
            value={newAddress}
            onChange={(e) => {
              setNewAddress(e.target.value);
              if (addressError) setAddressError(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                submitAddress();
              }
            }}
            placeholder={t("trustedSenders.addressPlaceholder")}
            className="flex-1 rounded-md px-2 py-1 text-sm outline-none"
            style={{
              background: "var(--bg-base)",
              color: "var(--fg-base)",
              border: `1px solid ${
                addressError ? "#ef4444" : "var(--border-base)"
              }`,
            }}
          />
          <button
            type="button"
            onClick={submitAddress}
            disabled={!newAddress.trim()}
            className="rounded-md border px-3 py-1 text-xs disabled:opacity-50"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
              background: "transparent",
            }}
          >
            {t("trustedSenders.add")}
          </button>
        </div>
        {addressError && (
          <p className="text-[11px]" style={{ color: "#ef4444" }}>
            {addressError}
          </p>
        )}
        {sortedAddresses.length === 0 ? (
          <p
            className="rounded-md border px-3 py-2 text-[11px]"
            style={{
              borderColor: "var(--border-soft)",
              color: "var(--fg-subtle)",
            }}
          >
            {t("trustedSenders.addressesEmpty")}
          </p>
        ) : (
          <ul className="flex flex-col gap-1">
            {sortedAddresses.map((email) => (
              <li
                key={email}
                className="flex items-center justify-between rounded-md border px-2.5 py-1 text-xs"
                style={{ borderColor: "var(--border-soft)" }}
              >
                <span className="truncate" title={email}>
                  {email}
                </span>
                <button
                  type="button"
                  onClick={() => removeTrustedSender(email)}
                  aria-label={t("trustedSenders.remove")}
                  title={t("trustedSenders.remove")}
                  className="ml-2 rounded px-1.5 py-0.5 text-[11px]"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  ✕
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      {/* Domain allowlist */}
      <section className="flex flex-col gap-2">
        <h3 className="text-sm font-medium">
          {t("trustedSenders.domainsHeading")}
        </h3>
        <p className="text-[11px]" style={{ color: "var(--fg-muted)" }}>
          {t("trustedSenders.domainsHint")}
        </p>
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={newDomain}
            onChange={(e) => {
              setNewDomain(e.target.value);
              if (domainError) setDomainError(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                submitDomain();
              }
            }}
            placeholder={t("trustedSenders.domainPlaceholder")}
            className="flex-1 rounded-md px-2 py-1 text-sm outline-none"
            style={{
              background: "var(--bg-base)",
              color: "var(--fg-base)",
              border: `1px solid ${
                domainError ? "#ef4444" : "var(--border-base)"
              }`,
            }}
          />
          <button
            type="button"
            onClick={submitDomain}
            disabled={!newDomain.trim()}
            className="rounded-md border px-3 py-1 text-xs disabled:opacity-50"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
              background: "transparent",
            }}
          >
            {t("trustedSenders.add")}
          </button>
        </div>
        {domainError && (
          <p className="text-[11px]" style={{ color: "#ef4444" }}>
            {domainError}
          </p>
        )}
        {sortedDomains.length === 0 ? (
          <p
            className="rounded-md border px-3 py-2 text-[11px]"
            style={{
              borderColor: "var(--border-soft)",
              color: "var(--fg-subtle)",
            }}
          >
            {t("trustedSenders.domainsEmpty")}
          </p>
        ) : (
          <ul className="flex flex-col gap-1">
            {sortedDomains.map((domain) => (
              <li
                key={domain}
                className="flex items-center justify-between rounded-md border px-2.5 py-1 text-xs"
                style={{ borderColor: "var(--border-soft)" }}
              >
                <span className="truncate" title={domain}>
                  @{domain}
                </span>
                <button
                  type="button"
                  onClick={() => removeTrustedDomain(domain)}
                  aria-label={t("trustedSenders.remove")}
                  title={t("trustedSenders.remove")}
                  className="ml-2 rounded px-1.5 py-0.5 text-[11px]"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  ✕
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
