import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type {
  AccountSummary,
  ApplyRuleResult,
  RuleDraft,
  RuleMatch,
  SpamPatternType,
  SpamRule,
} from "../../types";

type Props = {
  accounts: AccountSummary[];
};

const PATTERN_TYPES: { value: SpamPatternType; labelKey: string }[] = [
  { value: "from_email", labelKey: "spam.patternType.fromEmail" },
  { value: "from_domain", labelKey: "spam.patternType.fromDomain" },
  { value: "subject_contains", labelKey: "spam.patternType.subjectContains" },
  { value: "subject_regex", labelKey: "spam.patternType.subjectRegex" },
  { value: "body_contains", labelKey: "spam.patternType.bodyContains" },
  { value: "header_contains", labelKey: "spam.patternType.headerContains" },
];

/**
 * Settings panel for the spam rule set. Three parts:
 *   - list of existing rules with enable toggle + delete
 *   - "new rule" form (manual path — no pi yet; that comes in the next
 *     commit with a banner + learn-dialog)
 *   - preview / apply path so a user can see what a rule would do
 *     before committing it
 */
export function SpamRulesSettings({ accounts }: Props) {
  const { t } = useTranslation();
  const [rules, setRules] = useState<SpamRule[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Form state for creating a new rule.
  const [draft, setDraft] = useState<RuleDraft>({
    patternType: "from_domain",
    pattern: "",
    accountId: null,
  });
  const [previewRows, setPreviewRows] = useState<RuleMatch[] | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [applying, setApplying] = useState(false);
  const [lastApply, setLastApply] = useState<ApplyRuleResult | null>(null);

  const refresh = useCallback(async () => {
    try {
      const list = await invoke<SpamRule[]>("list_spam_rules");
      setRules(list);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const canPreview = useMemo(() => draft.pattern.trim().length > 0, [draft]);

  const onPreview = async () => {
    setPreviewing(true);
    setError(null);
    try {
      const rows = await invoke<RuleMatch[]>("preview_spam_rule", { draft });
      setPreviewRows(rows);
    } catch (e) {
      setError(String(e));
      setPreviewRows(null);
    } finally {
      setPreviewing(false);
    }
  };

  const onApply = async () => {
    setApplying(true);
    setError(null);
    setLastApply(null);
    try {
      const result = await invoke<ApplyRuleResult>("apply_spam_rule", { draft });
      setLastApply(result);
      setPreviewRows(null);
      setDraft({ patternType: draft.patternType, pattern: "", accountId: null });
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setApplying(false);
    }
  };

  const onToggle = async (rule: SpamRule) => {
    try {
      await invoke("set_spam_rule_enabled", {
        ruleId: rule.id,
        enabled: !rule.enabled,
      });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const onDelete = async (rule: SpamRule) => {
    if (!window.confirm(t("spam.confirmDelete", { pattern: rule.pattern }))) {
      return;
    }
    try {
      await invoke("delete_spam_rule", { ruleId: rule.id });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="flex flex-col gap-5">
      <header>
        <h2 className="text-base font-semibold">{t("spam.title")}</h2>
        <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("spam.hint")}
        </p>
      </header>

      {/* New rule form */}
      <section
        className="rounded-md border p-3"
        style={{
          borderColor: "var(--border-base)",
          background: "var(--bg-base)",
        }}
      >
        <div className="mb-3 text-sm font-medium">{t("spam.newRule")}</div>
        <div className="grid grid-cols-[auto_1fr_auto] items-end gap-2">
          <label className="text-xs" style={{ color: "var(--fg-muted)" }}>
            <span className="mb-1 block">{t("spam.patternTypeLabel")}</span>
            <select
              value={draft.patternType}
              onChange={(e) =>
                setDraft((d) => ({
                  ...d,
                  patternType: e.target.value as SpamPatternType,
                }))
              }
              className="rounded-md px-2 py-1.5 text-sm"
              style={{
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-base)",
              }}
            >
              {PATTERN_TYPES.map((p) => (
                <option key={p.value} value={p.value}>
                  {t(p.labelKey)}
                </option>
              ))}
            </select>
          </label>
          <label className="text-xs" style={{ color: "var(--fg-muted)" }}>
            <span className="mb-1 block">{t("spam.patternLabel")}</span>
            <input
              value={draft.pattern}
              onChange={(e) =>
                setDraft((d) => ({ ...d, pattern: e.target.value }))
              }
              placeholder={placeholderFor(draft.patternType)}
              className="w-full rounded-md px-2 py-1.5 text-sm"
              style={{
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-base)",
              }}
            />
          </label>
          <label className="text-xs" style={{ color: "var(--fg-muted)" }}>
            <span className="mb-1 block">{t("spam.accountLabel")}</span>
            <select
              value={draft.accountId ?? ""}
              onChange={(e) =>
                setDraft((d) => ({
                  ...d,
                  accountId: e.target.value === "" ? null : e.target.value,
                }))
              }
              className="rounded-md px-2 py-1.5 text-sm"
              style={{
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-base)",
              }}
            >
              <option value="">{t("spam.globalScope")}</option>
              {accounts.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.displayName}
                </option>
              ))}
            </select>
          </label>
        </div>

        <div className="mt-3 flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={onPreview}
            disabled={!canPreview || previewing}
            className="rounded-md border px-3 py-1.5 text-xs disabled:opacity-50"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
            }}
          >
            {previewing ? t("spam.previewing") : t("spam.previewButton")}
          </button>
          <button
            type="button"
            onClick={onApply}
            disabled={!canPreview || applying}
            className="rounded-md px-3 py-1.5 text-xs font-medium disabled:opacity-50"
            style={{ background: "var(--accent)", color: "white" }}
          >
            {applying ? t("spam.applying") : t("spam.applyButton")}
          </button>
          {previewRows && (
            <span className="text-xs" style={{ color: "var(--fg-muted)" }}>
              {t("spam.previewHitCount", { count: previewRows.length })}
            </span>
          )}
        </div>

        {previewRows && previewRows.length > 0 && (
          <ul
            className="mt-3 max-h-48 overflow-y-auto rounded-md border"
            style={{ borderColor: "var(--border-soft)" }}
          >
            {previewRows.map((m) => (
              <li
                key={m.messageId}
                className="border-b px-3 py-1.5 text-xs last:border-b-0"
                style={{ borderColor: "var(--border-soft)" }}
              >
                <div
                  className="truncate"
                  style={{ color: "var(--fg-base)" }}
                >
                  {m.subject || t("spam.noSubject")}
                </div>
                <div
                  className="truncate text-[10px]"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {m.fromEmail} · {m.folderName}
                </div>
              </li>
            ))}
          </ul>
        )}

        {lastApply && (
          <div
            className="mt-3 rounded-md px-3 py-2 text-xs"
            style={{
              background: "rgba(16,185,129,0.12)",
              color: "#10b981",
              border: "1px solid rgba(16,185,129,0.25)",
            }}
          >
            {t("spam.appliedSummary", {
              moved: lastApply.moved,
              matched: lastApply.matched,
            })}
            {lastApply.alreadyInSpam > 0 && (
              <>
                {" "}
                {t("spam.appliedAlreadyInSpam", {
                  count: lastApply.alreadyInSpam,
                })}
              </>
            )}
          </div>
        )}
      </section>

      {/* Existing rules */}
      <section>
        <div className="mb-2 text-xs uppercase tracking-wider" style={{ color: "var(--fg-subtle)" }}>
          {t("spam.existingRules")}
        </div>
        {rules === null ? (
          <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
            …
          </p>
        ) : rules.length === 0 ? (
          <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
            {t("spam.noRules")}
          </p>
        ) : (
          <ul
            className="flex flex-col overflow-hidden rounded-md border"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-base)",
            }}
          >
            {rules.map((r, i) => {
              const scope =
                r.accountId === null
                  ? t("spam.globalScope")
                  : (accounts.find((a) => a.id === r.accountId)?.displayName ??
                    r.accountId);
              return (
                <li
                  key={r.id}
                  className={i === 0 ? "" : "border-t"}
                  style={{ borderColor: "var(--border-soft)" }}
                >
                  <div className="flex items-center gap-3 px-3 py-2.5 text-sm">
                    <input
                      type="checkbox"
                      checked={r.enabled}
                      onChange={() => void onToggle(r)}
                      title={t("spam.enabledToggle")}
                    />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-baseline gap-2">
                        <span
                          className="rounded bg-black/5 px-1.5 py-0.5 text-[10px] font-semibold uppercase"
                          style={{ color: "var(--fg-muted)" }}
                        >
                          {t(`spam.patternType.${camel(r.patternType)}`)}
                        </span>
                        <span
                          className="truncate font-mono text-[12px]"
                          style={{
                            color: r.enabled ? "var(--fg-base)" : "var(--fg-subtle)",
                            textDecoration: r.enabled ? "none" : "line-through",
                          }}
                        >
                          {r.pattern}
                        </span>
                      </div>
                      <div
                        className="mt-0.5 truncate text-[11px]"
                        style={{ color: "var(--fg-subtle)" }}
                        title={r.reason ?? undefined}
                      >
                        {scope} ·{" "}
                        {t("spam.hitCount", { count: r.hitCount })}
                        {r.confidence !== null && (
                          <>
                            {" · "}
                            {t("spam.confidence", {
                              percent: Math.round(r.confidence * 100),
                            })}
                          </>
                        )}
                      </div>
                    </div>
                    <button
                      type="button"
                      onClick={() => void onDelete(r)}
                      className="rounded-md border px-2 py-1 text-[11px]"
                      style={{
                        borderColor: "var(--border-base)",
                        color: "var(--fg-muted)",
                      }}
                    >
                      ✕
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      {error && (
        <div
          className="rounded-md px-3 py-2 text-xs"
          style={{
            background: "rgba(248,113,113,0.12)",
            color: "#ef4444",
            border: "1px solid rgba(248,113,113,0.25)",
          }}
        >
          {error}
        </div>
      )}
    </div>
  );
}

function placeholderFor(t: SpamPatternType): string {
  switch (t) {
    case "from_email":
      return "promo@example.xyz";
    case "from_domain":
      return "example.xyz";
    case "subject_contains":
      return "50% Rabatt";
    case "subject_regex":
      return "(?i)^(RE: )?\\[?SPAM\\]?";
    case "body_contains":
      return "click here now";
    case "header_contains":
      return "x-spam-status: yes";
  }
}

function camel(s: string): string {
  return s.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
}
