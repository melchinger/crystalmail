// Pre-Apply-Dialog für Workflows mit `ParamSource: "prompt"`-Schritten.
// Sammelt vor dem eigentlichen `apply_workflow`-Call alle vom User
// einzugebenden Werte ein, damit das Backend keine Pause-Resume-IPC
// braucht.
//
// Pre-Fill:
//   * `defaultTemplate` aus dem Param wird in die Eingabe vorgeblendet.
//   * Statische Template-Variablen ($subject/$from/$date) werden gegen
//     die Envelope-Daten aufgelöst, damit der User direkt den
//     finalen Wert sieht (z.B. „Daily Report" statt „$subject").
//   * Dynamische Variablen ($csv/$attachments_dir/$body_md) bleiben
//     als Platzhalter stehen — die kennen wir erst zur Apply-Zeit.

import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { EnvelopeSummary, ScriptParam, Workflow } from "../types";

/** Frontend-leichte Variante des Backend-substitute. Resolvt nur das,
 *  was wir hier sicher wissen (Envelope-Felder + Date-Varianten).
 *  Dynamische Pfad-Vars wie `$csv`/`$attachments_dir`/`$body_md`
 *  passieren unverändert durch — Backend macht den Rest beim Apply.
 *
 *  Format-Strings hier müssen 1:1 mit `application/workflows.rs:lookup`
 *  übereinstimmen, sonst sieht der User im Dialog einen anderen Wert
 *  als im finalen Skript-Argument. */
function substituteEnvelopeVars(
  s: string | null | undefined,
  envelope: EnvelopeSummary,
): string {
  if (!s) return "";
  // Mail-Datum lokal — JS rechnet automatisch um, wenn der ISO-String
  // eine Zone trägt.
  const d = new Date(envelope.date);
  const pad = (n: number) => String(n).padStart(2, "0");
  const Y = d.getFullYear();
  const M = pad(d.getMonth() + 1);
  const D = pad(d.getDate());
  const h = pad(d.getHours());
  const m = pad(d.getMinutes());
  const s2 = pad(d.getSeconds());
  return s.replace(/\$([A-Za-z_][A-Za-z0-9_]*)/g, (whole, name) => {
    switch (name) {
      case "subject":
        return envelope.subject;
      case "from":
        return envelope.fromFirst;
      case "date":
        return envelope.date;
      case "date_iso":
        return `${Y}-${M}-${D}`;
      case "date_de":
        return `${D}.${M}.${Y}`;
      case "datetime":
        return `${Y}-${M}-${D} ${h}:${m}`;
      case "datetime_seconds":
        return `${Y}-${M}-${D} ${h}:${m}:${s2}`;
      case "datetime_iso":
        return `${Y}-${M}-${D}T${h}:${m}`;
      case "datetime_compact":
        return `${Y}${M}${D}-${h}${m}`;
      case "time":
        return `${h}:${m}`;
      case "time_seconds":
        return `${h}:${m}:${s2}`;
      case "year":
        return String(Y);
      case "month":
        return M;
      case "day":
        return D;
      default:
        return whole;
    }
  });
}

/** Param-Schritte mit prompt-Source aus einem Workflow extrahieren —
 *  in die Reihenfolge bringen, in der sie im Workflow stehen. */
export function collectPromptParams(workflow: Workflow): ScriptParam[] {
  const out: ScriptParam[] = [];
  for (const step of workflow.steps) {
    if (step.type !== "runScript") continue;
    for (const p of step.parameters) {
      if (p.enabled && p.source.kind === "prompt") {
        out.push(p);
      }
    }
  }
  return out;
}

type Props = {
  workflow: Workflow;
  envelope: EnvelopeSummary;
  onCancel: () => void;
  onSubmit: (values: Record<string, string>) => void;
};

export function WorkflowPromptDialog({
  workflow,
  envelope,
  onCancel,
  onSubmit,
}: Props) {
  const { t } = useTranslation();
  const params = useMemo(() => collectPromptParams(workflow), [workflow]);

  // Initial-Werte: Default-Template auflösen (statische Vars), Rest leer.
  const [values, setValues] = useState<Record<string, string>>(() => {
    const init: Record<string, string> = {};
    for (const p of params) {
      const dt = p.source.kind === "prompt" ? p.source.defaultTemplate : null;
      init[p.key] = substituteEnvelopeVars(dt, envelope);
    }
    return init;
  });

  const allRequiredFilled = params.every(
    (p) => !p.required || (values[p.key] ?? "").trim().length > 0,
  );

  const submit = () => {
    if (!allRequiredFilled) return;
    onSubmit(values);
  };

  return (
    <div
      className="fixed inset-0 z-[63] flex items-start justify-center overflow-y-auto px-4 py-[12vh]"
      style={{ background: "rgba(0,0,0,0.55)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onCancel();
      }}
    >
      <div
        role="dialog"
        className="flex w-full max-w-lg flex-col overflow-hidden rounded-xl border shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
          if (e.key === "Escape") onCancel();
        }}
      >
        <header
          className="flex items-center justify-between border-b px-4 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <h2 className="text-sm font-semibold">
            {t("workflows.promptDialog.title", { name: workflow.name })}
          </h2>
          <button
            type="button"
            onClick={onCancel}
            className="text-xs"
            style={{ color: "var(--fg-muted)" }}
          >
            ✕
          </button>
        </header>
        <div className="flex flex-col gap-3 px-4 py-4">
          <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
            {t("workflows.promptDialog.intro")}
          </p>
          {params.length === 0 && (
            // Defensiv — sollte nicht erreichbar sein, weil Caller nur
            // aufmacht wenn collectPromptParams was liefert.
            <p
              className="text-xs italic"
              style={{ color: "var(--fg-subtle)" }}
            >
              {t("workflows.promptDialog.noPrompts")}
            </p>
          )}
          {params.map((p) => {
            const required = p.required;
            const value = values[p.key] ?? "";
            const showError = required && value.trim().length === 0;
            return (
              <div key={p.key} className="flex flex-col gap-1">
                <label
                  className="text-xs"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {p.label}
                  {required && (
                    <span style={{ color: "#ef4444" }} aria-hidden>
                      {" *"}
                    </span>
                  )}
                </label>
                {p.valueType === "choice" && p.choices.length > 0 ? (
                  <select
                    value={value}
                    onChange={(e) =>
                      setValues({ ...values, [p.key]: e.target.value })
                    }
                    className="rounded-md border px-2 py-1 text-sm"
                    style={{
                      borderColor: showError
                        ? "#ef4444"
                        : "var(--border-base)",
                      background: "var(--bg-base)",
                      color: "var(--fg-base)",
                    }}
                  >
                    <option value="">—</option>
                    {p.choices.map((c) => (
                      <option key={c} value={c}>
                        {c}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    type={p.valueType === "number" ? "number" : "text"}
                    autoFocus={p === params[0]}
                    value={value}
                    onChange={(e) =>
                      setValues({ ...values, [p.key]: e.target.value })
                    }
                    placeholder={p.defaultValue ?? ""}
                    className="rounded-md border px-2 py-1.5 text-sm"
                    style={{
                      borderColor: showError
                        ? "#ef4444"
                        : "var(--border-base)",
                      background: "var(--bg-base)",
                      color: "var(--fg-base)",
                    }}
                  />
                )}
                {p.helpText && (
                  <span
                    className="text-[11px]"
                    style={{ color: "var(--fg-subtle)" }}
                  >
                    {p.helpText}
                  </span>
                )}
              </div>
            );
          })}
        </div>
        <footer
          className="flex items-center justify-between gap-2 border-t px-4 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <span
            className="text-[11px]"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("workflows.promptDialog.submitHint")}
          </span>
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={onCancel}
              className="rounded-md border px-3 py-1 text-xs"
              style={{
                borderColor: "var(--border-base)",
                color: "var(--fg-muted)",
              }}
            >
              {t("workflows.promptDialog.cancel")}
            </button>
            <button
              type="button"
              onClick={submit}
              disabled={!allRequiredFilled}
              className="rounded-md border px-3 py-1 text-xs"
              style={{
                borderColor: "var(--border-base)",
                background: allRequiredFilled
                  ? "var(--accent)"
                  : "var(--bg-base)",
                color: allRequiredFilled
                  ? "var(--bg-panel)"
                  : "var(--fg-muted)",
                opacity: allRequiredFilled ? 1 : 0.6,
              }}
            >
              {t("workflows.promptDialog.run")}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}
