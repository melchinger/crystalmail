import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { PiConfig, PiModel } from "../../types";

/**
 * Settings panel for the `pi` agent subprocess. Fields are thin wrappers
 * around the Rust-side `PiConfig` struct — bin path, provider, model, tools
 * whitelist, thinking mode. Save triggers a subprocess reset so the next
 * ask respawns with the new config.
 */
export function PiSettings() {
  const { t } = useTranslation();
  const [cfg, setCfg] = useState<PiConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  // Model picker state: `null` = never loaded yet, `[]` = loaded but empty
  // (unlikely — usually means pi returned no parsable rows).
  const [models, setModels] = useState<PiModel[] | null>(null);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [modelsError, setModelsError] = useState<string | null>(null);
  // Custom models maintained by the user — for providers whose inventory
  // pi doesn't expose in `agent/models.json` (e.g. kimi-coder lives in a
  // separate pi config file). Persisted in localStorage so it survives
  // app restarts without a backend round-trip.
  const [customModels, setCustomModels] = useState<PiModel[]>(() =>
    loadCustomModels(),
  );
  const [newProvider, setNewProvider] = useState("");
  const [newModel, setNewModel] = useState("");

  const addCustom = () => {
    const p = newProvider.trim();
    const n = newModel.trim();
    if (!p || !n) return;
    // Dedupe across both the pi-loaded list and existing customs. If the
    // entry already exists we silently skip + clear the inputs; no noisy
    // error for a no-op.
    const all = [...(models ?? []), ...customModels];
    if (all.some((m) => m.provider === p && m.name === n)) {
      setNewProvider("");
      setNewModel("");
      return;
    }
    const next = [...customModels, { provider: p, name: n, active: false }];
    setCustomModels(next);
    saveCustomModels(next);
    setNewProvider("");
    setNewModel("");
  };

  const removeCustom = (provider: string, name: string) => {
    const next = customModels.filter(
      (m) => !(m.provider === provider && m.name === name),
    );
    setCustomModels(next);
    saveCustomModels(next);
  };

  const isCustomEntry = (m: PiModel) =>
    customModels.some((c) => c.provider === m.provider && c.name === m.name);

  const loadModels = async () => {
    setModelsLoading(true);
    setModelsError(null);
    try {
      const list = await invoke<PiModel[]>("list_pi_models");
      setModels(list);
    } catch (e) {
      setModelsError(String(e));
    } finally {
      setModelsLoading(false);
    }
  };

  // Group by provider for a cleaner picker — easier to eyeball "ok,
  // these are my local ollama models, these are the cloud ones".
  // Custom entries are merged in regardless of whether the backend
  // list has been loaded, so a user who hasn't clicked "Modelle laden"
  // still sees their own additions.
  const modelsByProvider = useMemo(() => {
    const hasAny = (models?.length ?? 0) > 0 || customModels.length > 0;
    if (!hasAny) return null;
    const map = new Map<string, PiModel[]>();
    for (const m of models ?? []) {
      const arr = map.get(m.provider) ?? [];
      arr.push(m);
      map.set(m.provider, arr);
    }
    for (const m of customModels) {
      const arr = map.get(m.provider) ?? [];
      // Avoid duplicating if pi has since started exposing the same
      // entry — the backend list wins for display, but the custom
      // record stays behind (untouched in localStorage) so the user
      // can still remove it if they want.
      if (!arr.some((x) => x.name === m.name)) {
        arr.push(m);
      }
      map.set(m.provider, arr);
    }
    return map;
  }, [models, customModels]);

  useEffect(() => {
    invoke<PiConfig>("get_pi_config")
      .then(setCfg)
      .catch((e) => setError(String(e)));
  }, []);

  if (!cfg) {
    return (
      <p className="text-sm" style={{ color: "var(--fg-muted)" }}>
        {t("settings.pi.loading")}
      </p>
    );
  }

  const patch = (p: Partial<PiConfig>) =>
    setCfg((c) => (c ? { ...c, ...p } : c));

  const save = async () => {
    setSaving(true);
    setError(null);
    setInfo(null);
    try {
      await invoke("set_pi_config", { config: cfg });
      setInfo(t("settings.pi.saved"));
      window.setTimeout(() => setInfo(null), 2500);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  /**
   * Master kill-switch. Goes through its own command (`set_ai_enabled`)
   * rather than the full `set_pi_config` save path because:
   *   * we want it to take effect *immediately*, not on Save click
   *   * it shouldn't kill an interactive chat — `set_pi_config`
   *     respawns the pi process; `set_ai_enabled` doesn't
   *
   * Optimistic flip: state goes first, the backend call follows. On
   * failure the state rolls back. Banner above the form makes it
   * unmissable when AI is currently off.
   */
  const toggleAi = async () => {
    if (!cfg) return;
    const next = !cfg.enabled;
    patch({ enabled: next });
    try {
      await invoke("set_ai_enabled", { enabled: next });
      // Notify any listeners (status-bar indicator etc.) that the
      // flag changed without forcing them to re-poll. Plain custom
      // event keeps the wiring out of React state plumbing.
      window.dispatchEvent(
        new CustomEvent("cm:ai-enabled-changed", { detail: { enabled: next } }),
      );
    } catch (e) {
      patch({ enabled: !next });
      setError(String(e));
    }
  };

  return (
    <div className="flex flex-col gap-5">
      <header>
        <h2 className="text-base font-semibold">{t("settings.pi.title")}</h2>
        <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.pi.hint")}
        </p>
      </header>

      {/* Master AI kill-switch. Lives at the top so the toggle is the
          first thing the user sees when opening Pi settings, and so the
          banner-style off-state is impossible to miss. */}
      <div
        className="flex items-center gap-3 rounded-md border px-3 py-2"
        style={{
          borderColor: cfg.enabled
            ? "var(--border-base)"
            : "rgba(239,68,68,0.45)",
          background: cfg.enabled
            ? "var(--bg-base)"
            : "rgba(239,68,68,0.10)",
        }}
      >
        <label className="flex flex-1 cursor-pointer items-center gap-3">
          <input
            type="checkbox"
            checked={cfg.enabled}
            onChange={() => void toggleAi()}
            className="h-4 w-4 cursor-pointer"
          />
          <div className="flex flex-1 flex-col">
            <span className="text-sm font-semibold">
              {t("settings.pi.aiMasterToggle")}
            </span>
            <span
              className="text-[11px]"
              style={{ color: "var(--fg-muted)" }}
            >
              {cfg.enabled
                ? t("settings.pi.aiMasterOnHint")
                : t("settings.pi.aiMasterOffHint")}
            </span>
          </div>
          <span
            className="rounded px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider"
            style={{
              background: cfg.enabled
                ? "rgba(34,197,94,0.15)"
                : "rgba(239,68,68,0.20)",
              color: cfg.enabled ? "#16a34a" : "#ef4444",
            }}
          >
            {cfg.enabled
              ? t("settings.pi.aiMasterOn")
              : t("settings.pi.aiMasterOff")}
          </span>
        </label>
      </div>

      <Field label={t("settings.pi.binPath")} hint={t("settings.pi.binPathHint")}>
        <input
          value={cfg.binPath}
          onChange={(e) => patch({ binPath: e.target.value })}
          className={inputCls}
          placeholder="pi"
        />
      </Field>

      <div className="grid grid-cols-[1fr_1fr] gap-3">
        <Field label={t("settings.pi.provider")}>
          <select
            value={cfg.provider}
            onChange={(e) => patch({ provider: e.target.value })}
            className={inputCls}
          >
            <option value="ollama">ollama</option>
            <option value="openai">openai</option>
            <option value="anthropic">anthropic</option>
            <option value="cloud">cloud</option>
          </select>
        </Field>
        <Field label={t("settings.pi.model")} hint={t("settings.pi.modelHint")}>
          <input
            value={cfg.model}
            onChange={(e) => patch({ model: e.target.value })}
            className={inputCls}
            placeholder="gemma3"
          />
        </Field>
      </div>

      <Field label={t("settings.pi.tools")} hint={t("settings.pi.toolsHint")}>
        <input
          value={cfg.tools}
          onChange={(e) => patch({ tools: e.target.value })}
          className={inputCls}
          placeholder="read,grep,find,ls"
        />
      </Field>

      <Field
        label={t("settings.pi.promptPrefix")}
        hint={t("settings.pi.promptPrefixHint")}
      >
        <textarea
          value={cfg.promptPrefix}
          onChange={(e) => patch({ promptPrefix: e.target.value })}
          rows={5}
          placeholder={t("settings.pi.promptPrefixPlaceholder")}
          className="w-full resize-vertical rounded-md px-3 py-2 text-[13px] outline-none"
          style={{
            background: "var(--bg-base)",
            color: "var(--fg-base)",
            border: "1px solid var(--border-base)",
            minHeight: "110px",
          }}
        />
      </Field>

      <div className="grid grid-cols-[1fr_auto] items-end gap-3">
        <Field label={t("settings.pi.thinking")}>
          <select
            value={cfg.thinking}
            onChange={(e) => patch({ thinking: e.target.value })}
            className={inputCls}
          >
            <option value="off">off</option>
            <option value="low">low</option>
            <option value="medium">medium</option>
            <option value="high">high</option>
          </select>
        </Field>
        <label className="mb-2 inline-flex items-center gap-2 text-xs">
          <input
            type="checkbox"
            checked={cfg.showThinking}
            onChange={(e) => patch({ showThinking: e.target.checked })}
          />
          <span style={{ color: "var(--fg-muted)" }}>
            {t("settings.pi.showThinking")}
          </span>
        </label>
      </div>

      {/* Model-picker: probes `pi models` once so the user doesn't have
          to type exact slugs. Each row offers two targeting buttons —
          "für Chat übernehmen" writes into provider/model, "für Spam"
          writes into spamProvider/spamModel. No auto-save; user clicks
          "Speichern" at the bottom when satisfied. */}
      <section
        className="flex flex-col gap-2 rounded-md border px-4 py-3"
        style={{
          borderColor: "var(--border-soft)",
          background: "var(--bg-base)",
        }}
      >
        <header className="flex items-center justify-between gap-2">
          <div>
            <h3 className="text-sm font-semibold">
              {t("settings.pi.modelPicker")}
            </h3>
            <p
              className="mt-0.5 text-[11px]"
              style={{ color: "var(--fg-subtle)" }}
            >
              {t("settings.pi.modelPickerHint")}
            </p>
          </div>
          <button
            type="button"
            onClick={() => void loadModels()}
            disabled={modelsLoading}
            className="rounded-md border px-2.5 py-1 text-xs disabled:opacity-50"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
            }}
          >
            {modelsLoading
              ? t("settings.pi.modelPickerLoading")
              : models === null
                ? t("settings.pi.modelPickerLoad")
                : t("settings.pi.modelPickerReload")}
          </button>
        </header>

        {/* Current selection status — shows what the → Chat / → Spam
            buttons wrote into PiConfig, so the user doesn't have to
            hunt for checkmarks in the list. The ✕ on the Spam row
            clears the override so spam analysis falls back to the
            chat model. */}
        <div
          className="flex flex-col gap-1 rounded-md border px-3 py-2 text-[11px]"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-panel)",
          }}
        >
          <div className="flex items-center gap-2">
            <span
              className="w-12 shrink-0 uppercase tracking-wider"
              style={{ color: "var(--fg-subtle)" }}
            >
              Chat
            </span>
            <span className="font-mono" style={{ color: "var(--fg-base)" }}>
              {cfg.provider || "—"}
              {cfg.model ? ` / ${cfg.model}` : ""}
            </span>
          </div>
          <div className="flex items-center gap-2">
            <span
              className="w-12 shrink-0 uppercase tracking-wider"
              style={{ color: "var(--fg-subtle)" }}
            >
              Spam
            </span>
            {cfg.spamProvider || cfg.spamModel ? (
              <>
                <span
                  className="font-mono"
                  style={{ color: "var(--fg-base)" }}
                >
                  {cfg.spamProvider || cfg.provider || "—"}
                  {cfg.spamModel
                    ? ` / ${cfg.spamModel}`
                    : cfg.model
                      ? ` / ${cfg.model}`
                      : ""}
                </span>
                <button
                  type="button"
                  onClick={() =>
                    patch({ spamProvider: "", spamModel: "" })
                  }
                  title={t("settings.pi.spamResetTooltip")}
                  className="ml-1 rounded border px-1.5 text-[10px]"
                  style={{
                    borderColor: "var(--border-base)",
                    color: "var(--fg-muted)",
                  }}
                >
                  ✕
                </button>
              </>
            ) : (
              <span
                className="font-mono"
                style={{ color: "var(--fg-muted)" }}
              >
                {t("settings.pi.spamInheritsFromChat")}
              </span>
            )}
          </div>
        </div>

        {modelsError && (
          <div
            className="rounded-md px-3 py-2 text-[11px]"
            style={{
              background: "rgba(248,113,113,0.12)",
              color: "#ef4444",
              border: "1px solid rgba(248,113,113,0.25)",
            }}
          >
            {modelsError}
          </div>
        )}

        {modelsByProvider && modelsByProvider.size > 0 && (
          <ul
            className="flex max-h-56 flex-col overflow-y-auto rounded-md border"
            style={{ borderColor: "var(--border-soft)" }}
          >
            {Array.from(modelsByProvider.entries()).map(
              ([provider, list], gi) => (
                <li
                  key={provider}
                  className={gi === 0 ? "" : "border-t"}
                  style={{ borderColor: "var(--border-soft)" }}
                >
                  <div
                    className="px-3 py-1 text-[10px] uppercase tracking-wider"
                    style={{
                      color: "var(--fg-subtle)",
                      background: "var(--bg-panel)",
                    }}
                  >
                    {provider}
                  </div>
                  <ul>
                    {list.map((m) => {
                      const isChat =
                        cfg.provider === m.provider && cfg.model === m.name;
                      const isSpam =
                        cfg.spamProvider === m.provider &&
                        cfg.spamModel === m.name;
                      return (
                        <li
                          key={`${m.provider}/${m.name}`}
                          className="flex items-center gap-2 border-t px-3 py-1.5 text-[12px]"
                          style={{ borderColor: "var(--border-soft)" }}
                        >
                          <span
                            className="min-w-0 flex-1 truncate font-mono"
                            style={{ color: "var(--fg-base)" }}
                            title={m.name}
                          >
                            {m.name}
                          </span>
                          {m.active && (
                            <span
                              className="rounded bg-black/5 px-1 text-[10px]"
                              style={{ color: "var(--fg-muted)" }}
                              title={t("settings.pi.modelPickerActive")}
                            >
                              ★
                            </span>
                          )}
                          {isCustomEntry(m) && (
                            <span
                              className="rounded px-1 text-[10px]"
                              style={{
                                background: "rgba(234,179,8,0.15)",
                                color: "#ca8a04",
                              }}
                              title={t("settings.pi.customBadgeTooltip")}
                            >
                              {t("settings.pi.customBadge")}
                            </span>
                          )}
                          {isCustomEntry(m) && (
                            <button
                              type="button"
                              onClick={() => removeCustom(m.provider, m.name)}
                              aria-label={t("settings.pi.removeCustom")}
                              title={t("settings.pi.removeCustom")}
                              className="rounded-md border px-1.5 py-0.5 text-[10px]"
                              style={{
                                borderColor: "var(--border-base)",
                                color: "var(--fg-muted)",
                              }}
                            >
                              ✕
                            </button>
                          )}
                          <button
                            type="button"
                            onClick={() =>
                              patch({
                                provider: m.provider,
                                model: m.name,
                              })
                            }
                            className="rounded-md border px-2 py-0.5 text-[10px] disabled:opacity-50"
                            style={{
                              borderColor: isChat
                                ? "var(--accent)"
                                : "var(--border-base)",
                              color: isChat ? "var(--accent)" : "var(--fg-base)",
                            }}
                            disabled={isChat}
                          >
                            {isChat
                              ? t("settings.pi.modelPickerChatActive")
                              : t("settings.pi.modelPickerUseForChat")}
                          </button>
                          <button
                            type="button"
                            onClick={() =>
                              patch({
                                spamProvider: m.provider,
                                spamModel: m.name,
                              })
                            }
                            className="rounded-md border px-2 py-0.5 text-[10px] disabled:opacity-50"
                            style={{
                              borderColor: isSpam
                                ? "var(--accent)"
                                : "var(--border-base)",
                              color: isSpam ? "var(--accent)" : "var(--fg-base)",
                            }}
                            disabled={isSpam}
                          >
                            {isSpam
                              ? t("settings.pi.modelPickerSpamActive")
                              : t("settings.pi.modelPickerUseForSpam")}
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                </li>
              ),
            )}
          </ul>
        )}

        {/* Manual entry — for providers whose inventory isn't in
            agent/models.json (pi keeps some provider configs in
            separate files). Inputs are free-form because the user
            knows what slug pi will accept when they write
            `--provider X --model Y`. */}
        <div
          className="mt-2 flex flex-col gap-2 rounded-md border px-3 py-2"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-panel)",
          }}
        >
          <div
            className="text-[10px] uppercase tracking-wider"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("settings.pi.customAddTitle")}
          </div>
          <div className="flex items-end gap-2">
            <label className="flex-1 text-[11px]" style={{ color: "var(--fg-muted)" }}>
              <span className="mb-0.5 block">{t("settings.pi.customProvider")}</span>
              <input
                value={newProvider}
                onChange={(e) => setNewProvider(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    addCustom();
                  }
                }}
                placeholder="kimi-coder"
                className="w-full rounded-md px-2 py-1 text-[12px] outline-none"
                style={{
                  background: "var(--bg-base)",
                  color: "var(--fg-base)",
                  border: "1px solid var(--border-base)",
                }}
              />
            </label>
            <label className="flex-[2] text-[11px]" style={{ color: "var(--fg-muted)" }}>
              <span className="mb-0.5 block">{t("settings.pi.customModel")}</span>
              <input
                value={newModel}
                onChange={(e) => setNewModel(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    addCustom();
                  }
                }}
                placeholder="kimi-k2.5"
                className="w-full rounded-md px-2 py-1 text-[12px] outline-none"
                style={{
                  background: "var(--bg-base)",
                  color: "var(--fg-base)",
                  border: "1px solid var(--border-base)",
                }}
              />
            </label>
            <button
              type="button"
              onClick={addCustom}
              disabled={!newProvider.trim() || !newModel.trim()}
              className="rounded-md px-3 py-1 text-[11px] font-medium disabled:opacity-50"
              style={{ background: "var(--accent)", color: "white" }}
            >
              +
            </button>
          </div>
        </div>
      </section>

      {/* Spam-Analyse-Config used to live in its own section here —
          removed as redundant. The model picker's → Spam buttons
          write the same fields, and the status strip at the top of
          the picker shows what's currently set (plus a ✕ to clear
          the override and fall back to the chat model). */}

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
      {info && (
        <div
          className="text-xs"
          style={{ color: "#10b981" }}
        >
          {info}
        </div>
      )}

      <div className="flex justify-end">
        <button
          type="button"
          onClick={save}
          disabled={saving}
          className="rounded-md px-4 py-1.5 text-sm font-medium disabled:opacity-50"
          style={{ background: "var(--accent)", color: "white" }}
        >
          {saving ? t("settings.pi.saving") : t("settings.pi.save")}
        </button>
      </div>
    </div>
  );
}

/**
 * Local storage of user-authored model entries. Lives in the browser
 * (not the Rust side) because it's pure UI state — the backend doesn't
 * care where the slugs come from when it writes them into PiConfig.
 */
const CUSTOM_MODELS_KEY = "crystalmail:pi-custom-models";

function loadCustomModels(): PiModel[] {
  try {
    const raw = localStorage.getItem(CUSTOM_MODELS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (m): m is PiModel =>
        typeof m === "object" &&
        m !== null &&
        typeof (m as Record<string, unknown>).name === "string" &&
        typeof (m as Record<string, unknown>).provider === "string",
    ).map((m) => ({ ...m, active: false }));
  } catch {
    return [];
  }
}

function saveCustomModels(list: PiModel[]): void {
  try {
    localStorage.setItem(CUSTOM_MODELS_KEY, JSON.stringify(list));
  } catch {
    // localStorage may be disabled or quota-full — state stays
    // in-memory for the session, which is acceptable.
  }
}

const inputCls =
  "w-full rounded-md px-2.5 py-1.5 text-sm outline-none focus:ring-2";

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <label className="block">
      <span
        className="mb-1 block text-xs"
        style={{ color: "var(--fg-muted)" }}
      >
        {label}
      </span>
      <div className="[&>input]:bg-[var(--bg-base)] [&>input]:text-[var(--fg-base)] [&>input]:border [&>input]:border-[var(--border-base)] [&>select]:bg-[var(--bg-base)] [&>select]:text-[var(--fg-base)] [&>select]:border [&>select]:border-[var(--border-base)]">
        {children}
      </div>
      {hint && (
        <span
          className="mt-1 block text-[11px]"
          style={{ color: "var(--fg-subtle)" }}
        >
          {hint}
        </span>
      )}
    </label>
  );
}
