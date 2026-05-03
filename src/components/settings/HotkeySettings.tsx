import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  formatBinding,
  HOTKEY_ACTION_IDS,
  HOTKEY_ACTIONS,
  HOTKEY_DEFAULTS,
  normalizeKeyEvent,
  resetHotkeys,
  saveHotkeys,
  type HotkeyActionId,
  type HotkeyBindings,
} from "../../settings/hotkeys";

type Props = {
  bindings: HotkeyBindings;
  onChange: (next: HotkeyBindings) => void;
};

/**
 * Editable table of all hotkey actions, grouped by category. Each action
 * row shows its current binding(s) as pill chips plus an "+" to start
 * capturing a new one. Capturing is a one-shot listener: next key down
 * replaces the slot; Escape cancels.
 *
 * Conflict handling: when the captured combo is already bound to a
 * different action, that other action loses it on save — we don't let two
 * actions share a key.
 */
export function HotkeySettings({ bindings, onChange }: Props) {
  const { t } = useTranslation();
  // { action, slotIdx } while the user is capturing a replacement binding.
  // `slotIdx === -1` means "adding a new binding at the end of the list".
  const [capture, setCapture] = useState<
    { action: HotkeyActionId; slotIdx: number } | null
  >(null);

  const grouped = useMemo(() => {
    const message: HotkeyActionId[] = [];
    const app: HotkeyActionId[] = [];
    for (const id of HOTKEY_ACTION_IDS) {
      if (HOTKEY_ACTIONS[id].group === "message") message.push(id);
      else app.push(id);
    }
    return { message, app };
  }, []);

  const commit = (next: HotkeyBindings) => {
    saveHotkeys(next);
    onChange(next);
  };

  const onCaptured = (combo: string) => {
    if (!capture) return;
    // Strip the combo from any other action that currently owns it.
    const next: HotkeyBindings = {} as HotkeyBindings;
    for (const id of HOTKEY_ACTION_IDS) {
      next[id] =
        id === capture.action
          ? [...bindings[id]]
          : bindings[id].filter((b) => b !== combo);
    }
    if (capture.slotIdx < 0) {
      // Append — but dedupe if the key is already bound to this action.
      if (!next[capture.action].includes(combo)) {
        next[capture.action].push(combo);
      }
    } else {
      next[capture.action][capture.slotIdx] = combo;
      next[capture.action] = Array.from(new Set(next[capture.action]));
    }
    setCapture(null);
    commit(next);
  };

  const removeBinding = (action: HotkeyActionId, slotIdx: number) => {
    const next: HotkeyBindings = { ...bindings };
    next[action] = bindings[action].filter((_, i) => i !== slotIdx);
    commit(next);
  };

  const resetAll = () => {
    const ok = window.confirm(t("settings.hotkeys.resetConfirm"));
    if (!ok) return;
    onChange(resetHotkeys());
  };

  return (
    <div className="flex flex-col gap-6">
      <header className="flex items-start justify-between gap-4">
        <div>
          <h2 className="text-base font-semibold">
            {t("settings.hotkeys.title")}
          </h2>
          <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
            {t("settings.hotkeys.hint")}
          </p>
        </div>
        <button
          type="button"
          onClick={resetAll}
          className="rounded-md border px-3 py-1 text-xs"
          style={{
            borderColor: "var(--border-base)",
            color: "var(--fg-muted)",
          }}
        >
          {t("settings.hotkeys.reset")}
        </button>
      </header>

      <Group
        title={t("hotkeys.groups.message")}
        ids={grouped.message}
        bindings={bindings}
        capture={capture}
        onStartCapture={setCapture}
        onCaptured={onCaptured}
        onCancelCapture={() => setCapture(null)}
        onRemove={removeBinding}
      />
      <Group
        title={t("hotkeys.groups.app")}
        ids={grouped.app}
        bindings={bindings}
        capture={capture}
        onStartCapture={setCapture}
        onCaptured={onCaptured}
        onCancelCapture={() => setCapture(null)}
        onRemove={removeBinding}
      />
    </div>
  );
}

function Group({
  title,
  ids,
  bindings,
  capture,
  onStartCapture,
  onCaptured,
  onCancelCapture,
  onRemove,
}: {
  title: string;
  ids: HotkeyActionId[];
  bindings: HotkeyBindings;
  capture: { action: HotkeyActionId; slotIdx: number } | null;
  onStartCapture: (c: { action: HotkeyActionId; slotIdx: number }) => void;
  onCaptured: (combo: string) => void;
  onCancelCapture: () => void;
  onRemove: (action: HotkeyActionId, slotIdx: number) => void;
}) {
  const { t } = useTranslation();
  return (
    <section>
      <h3
        className="mb-2 text-[11px] uppercase tracking-[0.15em]"
        style={{ color: "var(--fg-subtle)" }}
      >
        {title}
      </h3>
      <ul
        className="flex flex-col divide-y rounded-md border"
        style={{ borderColor: "var(--border-base)" }}
      >
        {ids.map((id) => {
          const action = HOTKEY_ACTIONS[id];
          const slots = bindings[id];
          const defaults = HOTKEY_DEFAULTS[id];
          const isDefault =
            slots.length === defaults.length &&
            slots.every((s, i) => s === defaults[i]);

          return (
            <li
              key={id}
              className="grid grid-cols-[1fr_auto] items-center gap-3 px-3 py-2"
              style={{ borderColor: "var(--border-soft)" }}
            >
              <div className="min-w-0">
                <div className="text-sm" style={{ color: "var(--fg-base)" }}>
                  {t(action.labelKey)}
                </div>
                {!isDefault && (
                  <div
                    className="mt-0.5 text-[10px]"
                    style={{ color: "var(--fg-subtle)" }}
                  >
                    {t("settings.hotkeys.defaultWas", {
                      keys: defaults.map(formatBinding).join(", ") || "—",
                    })}
                  </div>
                )}
              </div>

              <div className="flex flex-wrap justify-end gap-1">
                {slots.map((combo, idx) => {
                  const active =
                    capture && capture.action === id && capture.slotIdx === idx;
                  return (
                    <BindingChip
                      key={`${combo}::${idx}`}
                      label={combo}
                      active={!!active}
                      onClick={() => onStartCapture({ action: id, slotIdx: idx })}
                      onRemove={() => onRemove(id, idx)}
                      onCaptured={onCaptured}
                      onCancel={onCancelCapture}
                    />
                  );
                })}
                <BindingChip
                  label={null}
                  active={
                    !!(
                      capture &&
                      capture.action === id &&
                      capture.slotIdx < 0
                    )
                  }
                  onClick={() => onStartCapture({ action: id, slotIdx: -1 })}
                  onCaptured={onCaptured}
                  onCancel={onCancelCapture}
                />
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

/**
 * Single binding chip. Three modes:
 *   * Bound + passive: shows the combo, click to rebind, ✕ to clear.
 *   * Adder (label === null): shows "+".
 *   * Active/capturing: highlights and listens for the next keydown.
 */
function BindingChip({
  label,
  active,
  onClick,
  onRemove,
  onCaptured,
  onCancel,
}: {
  label: string | null;
  active: boolean;
  onClick: () => void;
  onRemove?: () => void;
  onCaptured: (combo: string) => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const isAdder = label === null;

  return (
    <span
      className="inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-[11px] transition-colors"
      style={{
        borderColor: active ? "var(--accent)" : "var(--border-base)",
        background: active
          ? "var(--bg-selected)"
          : isAdder
            ? "transparent"
            : "var(--bg-base)",
        color: active
          ? "var(--accent)"
          : isAdder
            ? "var(--fg-muted)"
            : "var(--fg-base)",
      }}
    >
      <button
        type="button"
        onClick={onClick}
        onKeyDown={(e) => {
          if (!active) return;
          // Only react to complete key-downs with a real key.
          if (e.key === "Escape") {
            e.preventDefault();
            onCancel();
            return;
          }
          const combo = normalizeKeyEvent(e.nativeEvent);
          if (!combo) return; // pure modifier — keep listening
          e.preventDefault();
          e.stopPropagation();
          onCaptured(combo);
        }}
        // When active, keep the element focused so it captures all keys,
        // including ones that would otherwise bubble to `useHotkeys`.
        autoFocus={active}
        className="min-w-[3.5rem] rounded font-mono text-[11px] outline-none"
        style={{ color: "inherit" }}
      >
        {active
          ? t("settings.hotkeys.pressKey")
          : isAdder
            ? "+"
            : formatBinding(label!)}
      </button>
      {!isAdder && !active && onRemove && (
        <button
          type="button"
          onClick={onRemove}
          aria-label={t("settings.hotkeys.unbind")}
          className="rounded px-0.5 text-[10px]"
          style={{ color: "var(--fg-subtle)" }}
        >
          ✕
        </button>
      )}
    </span>
  );
}
