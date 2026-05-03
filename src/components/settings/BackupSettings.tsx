import { useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  open as openDialog,
  save as saveDialog,
} from "@tauri-apps/plugin-dialog";
import { downloadDir, documentDir, homeDir, join } from "@tauri-apps/api/path";

/**
 * Backup-Settings: Export/Import aller Konfigurationsdaten.
 *
 * Was eingepackt wird: Konten + Aliase + Spam-Regeln + Workflows +
 * pi/Workflow-Sidecar-Configs. Was draußen bleibt: zwischengespeicherte
 * Mails (regenerierbar via IMAP-Sync) und live-Trainingsdaten.
 *
 * Passwörter sind optional. Standardmäßig werden sie NICHT exportiert —
 * das Backup-File darf dann offen liegen, ohne dass Credentials lecken.
 * Wenn der User opt-in eine Passphrase angibt, werden die IMAP-Passwörter
 * mit Argon2id+ChaCha20-Poly1305 verschlüsselt eingebettet (siehe
 * `src-tauri/src/application/backup.rs`).
 *
 * Konflikt-Strategie beim Import: Konten mit gleicher Adresse werden
 * übersprungen — der User soll nicht versehentlich einen Account
 * überschreiben. Spam-Regeln und Workflows kommen mit ihren UUIDs;
 * Konflikte sind unwahrscheinlich, weil UUIDs eindeutig sind, aber wir
 * fail-loud falls sie auftreten (DB unique-constraint).
 */
type BackupPreview = {
  schemaVersion: number;
  exportedAt: string;
  crystalmailVersion: string;
  accountCount: number;
  aliasCount: number;
  spamRuleCount: number;
  workflowCount: number;
  workflowRuleCount: number;
  hasPiConfig: boolean;
  hasWorkflowConfig: boolean;
  hasEncryptedPasswords: boolean;
  /** E-Mail-Adressen, die hier schon mit anderer UUID existieren —
   *  werden beim Import übersprungen, samt ihrer FK-abhängigen Rules.
   *  UI zeigt die Liste als Warnblock vor dem Klick auf "Importieren". */
  conflictingAddresses: string[];
};

type ImportReport = {
  accountsAdded: number;
  accountsSkipped: number;
  aliasesAdded: number;
  spamRulesAdded: number;
  spamRulesSkipped: number;
  /** Spam-Regeln, deren Quell-Konto wegen Adress-Konflikt nicht
   *  importiert wurde — die Regel hängt also FK-mäßig in der Luft
   *  und wir verzichten auf den Import. */
  spamRulesSkippedUnknownAccount: number;
  workflowsAdded: number;
  workflowsSkipped: number;
  workflowRulesAdded: number;
  workflowRulesSkipped: number;
  workflowRulesSkippedUnknownAccount: number;
  passwordsRestored: number;
  piConfigRestored: boolean;
  workflowConfigRestored: boolean;
  skippedAddresses: string[];
  warnings: string[];
};

export function BackupSettings() {
  const { t } = useTranslation();

  // Export state. Two passphrase inputs because typos in a passphrase
  // that you'll never see again are a one-way trip to a useless backup.
  const [includePasswords, setIncludePasswords] = useState(false);
  const [passphrase, setPassphrase] = useState("");
  const [passphrase2, setPassphrase2] = useState("");
  const [exporting, setExporting] = useState(false);
  const [exportResult, setExportResult] = useState<
    { kind: "ok"; path: string } | { kind: "err"; msg: string } | null
  >(null);

  // Import state. The flow is two-step on purpose so the user sees what's
  // about to land BEFORE we touch the DB. `previewPath` carries the path
  // through to the import call so the user only picks the file once.
  const [importPath, setImportPath] = useState<string | null>(null);
  const [preview, setPreview] = useState<BackupPreview | null>(null);
  const [importPassphrase, setImportPassphrase] = useState("");
  const [importing, setImporting] = useState(false);
  const [importResult, setImportResult] = useState<
    { kind: "ok"; report: ImportReport } | { kind: "err"; msg: string } | null
  >(null);

  const passphraseValid =
    !includePasswords ||
    (passphrase.length >= 8 && passphrase === passphrase2);

  const startExport = async () => {
    if (!passphraseValid || exporting) return;
    setExporting(true);
    setExportResult(null);
    try {
      // Default-Pfad als ABSOLUTER Pfad — auf Windows muss der
      // Save-Dialog ein gültiges Verzeichnis kennen, sonst landet
      // die Auswahl in einem unvorhersagbaren Working-Directory
      // (oder schlägt mit "Pfad nicht gefunden" fehl). Reihenfolge:
      // Downloads → Documents → Home, je nachdem was sich auflösen
      // lässt. Falls alle scheitern (sehr exotisch), reicher
      // Filename-Only durch — der Dialog macht dann sein Bestes.
      let defaultPath = defaultBackupFilename();
      try {
        const dir =
          (await downloadDir().catch(() => null)) ??
          (await documentDir().catch(() => null)) ??
          (await homeDir().catch(() => null));
        if (dir) defaultPath = await join(dir, defaultPath);
      } catch {
        /* fall back to bare filename */
      }
      const dest = await saveDialog({
        defaultPath,
        title: t("settings.backup.exportDialogTitle"),
        // Tauri's Filter-API: Endungen WITHOUT dots, einzeln. Frühere
        // Form `["crystalmail-backup.json", "json"]` war kaputt — der
        // Dot in "crystalmail-backup.json" macht den Filter unbrauchbar.
        filters: [
          {
            name: t("settings.backup.fileTypeName"),
            extensions: ["json"],
          },
        ],
      });
      if (!dest) {
        setExporting(false);
        return;
      }
      await invoke("export_settings_to_path", {
        path: dest,
        passphrase: includePasswords ? passphrase : null,
      });
      setExportResult({ kind: "ok", path: dest });
      // Clear the passphrase from RAM as soon as the export is done —
      // we don't keep secrets longer than needed.
      setPassphrase("");
      setPassphrase2("");
    } catch (e) {
      setExportResult({ kind: "err", msg: String(e) });
    } finally {
      setExporting(false);
    }
  };

  const pickImportFile = async () => {
    setImportResult(null);
    setPreview(null);
    setImportPath(null);
    try {
      const path = await openDialog({
        multiple: false,
        title: t("settings.backup.importDialogTitle"),
        filters: [
          {
            name: t("settings.backup.fileTypeName"),
            // Endungen ohne Dots — Tauri-API-Konvention.
            extensions: ["json"],
          },
        ],
      });
      if (!path || typeof path !== "string") return;
      const p = await invoke<BackupPreview>("peek_backup_file", { path });
      setImportPath(path);
      setPreview(p);
    } catch (e) {
      setImportResult({ kind: "err", msg: String(e) });
    }
  };

  const runImport = async () => {
    if (!importPath || importing) return;
    setImporting(true);
    setImportResult(null);
    try {
      const report = await invoke<ImportReport>("import_settings_file", {
        path: importPath,
        passphrase:
          preview?.hasEncryptedPasswords && importPassphrase
            ? importPassphrase
            : null,
      });
      setImportResult({ kind: "ok", report });
      setPreview(null);
      setImportPath(null);
      setImportPassphrase("");
    } catch (e) {
      setImportResult({ kind: "err", msg: String(e) });
    } finally {
      setImporting(false);
    }
  };

  return (
    <div className="flex flex-col gap-5">
      <header>
        <h2 className="text-base font-semibold">
          {t("settings.backup.title")}
        </h2>
        <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.backup.hint")}
        </p>
      </header>

      {/* ─── Export ─────────────────────────────────────────────── */}
      <section
        className="flex flex-col gap-3 rounded-md border p-3"
        style={{
          borderColor: "var(--border-soft)",
          background: "var(--bg-base)",
        }}
      >
        <h3 className="text-sm font-semibold">
          {t("settings.backup.exportTitle")}
        </h3>
        <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.backup.exportHint")}
        </p>

        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={includePasswords}
            onChange={(e) => {
              setIncludePasswords(e.target.checked);
              if (!e.target.checked) {
                setPassphrase("");
                setPassphrase2("");
              }
            }}
          />
          <span>{t("settings.backup.includePasswords")}</span>
        </label>
        {includePasswords && (
          <div className="flex flex-col gap-2 pl-6">
            <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
              {t("settings.backup.passphraseHint")}
            </p>
            <input
              type="password"
              autoComplete="new-password"
              placeholder={t("settings.backup.passphrasePlaceholder")}
              value={passphrase}
              onChange={(e) => setPassphrase(e.target.value)}
              className="rounded border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
              }}
            />
            <input
              type="password"
              autoComplete="new-password"
              placeholder={t("settings.backup.passphraseConfirmPlaceholder")}
              value={passphrase2}
              onChange={(e) => setPassphrase2(e.target.value)}
              className="rounded border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
              }}
            />
            {passphrase.length > 0 && passphrase.length < 8 && (
              <p
                className="text-[11px]"
                style={{ color: "var(--accent-warn, #d97706)" }}
              >
                {t("settings.backup.passphraseTooShort")}
              </p>
            )}
            {passphrase.length >= 8 &&
              passphrase2.length > 0 &&
              passphrase !== passphrase2 && (
                <p
                  className="text-[11px]"
                  style={{ color: "var(--accent-warn, #d97706)" }}
                >
                  {t("settings.backup.passphraseMismatch")}
                </p>
              )}
          </div>
        )}

        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={() => void startExport()}
            disabled={!passphraseValid || exporting}
            className="rounded-md border px-3 py-1.5 text-sm"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-panel)",
              color: passphraseValid && !exporting
                ? "var(--accent)"
                : "var(--fg-muted)",
              opacity: !passphraseValid || exporting ? 0.6 : 1,
            }}
          >
            {exporting
              ? t("settings.backup.exporting")
              : t("settings.backup.exportButton")}
          </button>
          {exportResult?.kind === "ok" && (
            <span
              className="text-xs"
              style={{ color: "var(--accent-ok, #059669)" }}
            >
              {t("settings.backup.exportOk", { path: exportResult.path })}
            </span>
          )}
          {exportResult?.kind === "err" && (
            <span
              className="text-xs"
              style={{ color: "var(--accent-err, #dc2626)" }}
            >
              {exportResult.msg}
            </span>
          )}
        </div>
      </section>

      {/* ─── Import ─────────────────────────────────────────────── */}
      <section
        className="flex flex-col gap-3 rounded-md border p-3"
        style={{
          borderColor: "var(--border-soft)",
          background: "var(--bg-base)",
        }}
      >
        <h3 className="text-sm font-semibold">
          {t("settings.backup.importTitle")}
        </h3>
        <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.backup.importHint")}
        </p>

        {!preview && (
          <div>
            <button
              type="button"
              onClick={() => void pickImportFile()}
              className="rounded-md border px-3 py-1.5 text-sm"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--bg-panel)",
                color: "var(--accent)",
              }}
            >
              {t("settings.backup.pickFileButton")}
            </button>
          </div>
        )}

        {preview && (
          <div
            className="flex flex-col gap-2 rounded border p-2 text-xs"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-panel)",
            }}
          >
            <div style={{ color: "var(--fg-subtle)" }}>
              {t("settings.backup.previewExportedAt", {
                when: new Date(preview.exportedAt).toLocaleString(),
                version: preview.crystalmailVersion,
              })}
            </div>
            <ul className="ml-1 grid grid-cols-2 gap-x-4 gap-y-0.5">
              <li>
                {t("settings.backup.previewAccounts", {
                  count: preview.accountCount,
                })}
              </li>
              <li>
                {t("settings.backup.previewAliases", {
                  count: preview.aliasCount,
                })}
              </li>
              <li>
                {t("settings.backup.previewSpamRules", {
                  count: preview.spamRuleCount,
                })}
              </li>
              <li>
                {t("settings.backup.previewWorkflows", {
                  count: preview.workflowCount,
                })}
              </li>
              <li>
                {t("settings.backup.previewWorkflowRules", {
                  count: preview.workflowRuleCount,
                })}
              </li>
              {preview.hasPiConfig && <li>{t("settings.backup.previewPiConfig")}</li>}
              {preview.hasWorkflowConfig && (
                <li>{t("settings.backup.previewWorkflowConfig")}</li>
              )}
              {preview.hasEncryptedPasswords && (
                <li style={{ color: "var(--accent)" }}>
                  {t("settings.backup.previewPasswords")}
                </li>
              )}
            </ul>

            {preview.conflictingAddresses.length > 0 && (
              <div
                className="rounded border p-2 text-[11px]"
                style={{
                  borderColor: "var(--accent-warn, #d97706)",
                  background: "rgba(217,119,6,0.08)",
                  color: "var(--fg-base)",
                }}
              >
                <div className="font-semibold mb-1">
                  {t("settings.backup.conflictsHeading", {
                    count: preview.conflictingAddresses.length,
                  })}
                </div>
                <ul className="ml-3 list-disc">
                  {preview.conflictingAddresses.map((addr) => (
                    <li key={addr}>{addr}</li>
                  ))}
                </ul>
                <p className="mt-1" style={{ color: "var(--fg-subtle)" }}>
                  {t("settings.backup.conflictsHint")}
                </p>
              </div>
            )}

            {preview.hasEncryptedPasswords && (
              <div className="flex flex-col gap-1 pt-1">
                <label
                  className="text-[11px]"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {t("settings.backup.importPassphraseLabel")}
                </label>
                <input
                  type="password"
                  autoComplete="off"
                  value={importPassphrase}
                  onChange={(e) => setImportPassphrase(e.target.value)}
                  placeholder={t("settings.backup.passphrasePlaceholder")}
                  className="rounded border px-2 py-1 text-sm"
                  style={{
                    borderColor: "var(--border-base)",
                    background: "var(--bg-base)",
                    color: "var(--fg-base)",
                  }}
                />
                <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
                  {t("settings.backup.importPassphraseHint")}
                </p>
              </div>
            )}

            <div className="flex gap-2 pt-1">
              <button
                type="button"
                onClick={() => void runImport()}
                disabled={
                  importing ||
                  (preview.hasEncryptedPasswords && importPassphrase.length === 0)
                }
                className="rounded-md border px-3 py-1.5 text-sm"
                style={{
                  borderColor: "var(--border-base)",
                  background: "var(--bg-base)",
                  color: "var(--accent)",
                  opacity: importing ? 0.6 : 1,
                }}
              >
                {importing
                  ? t("settings.backup.importing")
                  : t("settings.backup.importButton")}
              </button>
              <button
                type="button"
                onClick={() => {
                  setPreview(null);
                  setImportPath(null);
                  setImportPassphrase("");
                }}
                disabled={importing}
                className="rounded-md border px-3 py-1.5 text-sm"
                style={{
                  borderColor: "var(--border-base)",
                  background: "var(--bg-base)",
                  color: "var(--fg-muted)",
                }}
              >
                {t("settings.backup.cancel")}
              </button>
            </div>
          </div>
        )}

        {importResult?.kind === "ok" && (
          <div
            className="rounded border p-2 text-xs"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-panel)",
              color: "var(--fg-base)",
            }}
          >
            <div className="font-semibold">
              {t("settings.backup.importDone")}
            </div>
            <ul className="mt-1 grid grid-cols-2 gap-x-4 gap-y-0.5">
              <li>
                {t("settings.backup.reportAccountsAdded", {
                  count: importResult.report.accountsAdded,
                })}
              </li>
              {importResult.report.accountsSkipped > 0 && (
                <li>
                  {t("settings.backup.reportAccountsSkipped", {
                    count: importResult.report.accountsSkipped,
                  })}
                </li>
              )}
              <li>
                {t("settings.backup.reportSpamRulesAdded", {
                  count: importResult.report.spamRulesAdded,
                })}
              </li>
              {importResult.report.spamRulesSkipped > 0 && (
                <li>
                  {t("settings.backup.reportSpamRulesSkipped", {
                    count: importResult.report.spamRulesSkipped,
                  })}
                </li>
              )}
              {importResult.report.spamRulesSkippedUnknownAccount > 0 && (
                <li style={{ color: "var(--accent-warn, #d97706)" }}>
                  {t("settings.backup.reportSpamRulesSkippedUnknownAccount", {
                    count:
                      importResult.report.spamRulesSkippedUnknownAccount,
                  })}
                </li>
              )}
              <li>
                {t("settings.backup.reportWorkflowsAdded", {
                  count: importResult.report.workflowsAdded,
                })}
              </li>
              {importResult.report.workflowsSkipped > 0 && (
                <li>
                  {t("settings.backup.reportWorkflowsSkipped", {
                    count: importResult.report.workflowsSkipped,
                  })}
                </li>
              )}
              <li>
                {t("settings.backup.reportWorkflowRulesAdded", {
                  count: importResult.report.workflowRulesAdded,
                })}
              </li>
              {importResult.report.workflowRulesSkipped > 0 && (
                <li>
                  {t("settings.backup.reportWorkflowRulesSkipped", {
                    count: importResult.report.workflowRulesSkipped,
                  })}
                </li>
              )}
              {importResult.report.workflowRulesSkippedUnknownAccount > 0 && (
                <li style={{ color: "var(--accent-warn, #d97706)" }}>
                  {t(
                    "settings.backup.reportWorkflowRulesSkippedUnknownAccount",
                    {
                      count:
                        importResult.report
                          .workflowRulesSkippedUnknownAccount,
                    },
                  )}
                </li>
              )}
              {importResult.report.passwordsRestored > 0 && (
                <li>
                  {t("settings.backup.reportPasswordsRestored", {
                    count: importResult.report.passwordsRestored,
                  })}
                </li>
              )}
            </ul>
            {importResult.report.skippedAddresses.length > 0 && (
              <p
                className="mt-2 text-[11px]"
                style={{ color: "var(--fg-subtle)" }}
              >
                {t("settings.backup.skippedAddressesHint", {
                  list: importResult.report.skippedAddresses.join(", "),
                })}
              </p>
            )}
            {importResult.report.warnings.length > 0 && (
              <ul
                className="mt-2 list-disc pl-4 text-[11px]"
                style={{ color: "var(--accent-warn, #d97706)" }}
              >
                {importResult.report.warnings.map((w, i) => (
                  <li key={i}>{w}</li>
                ))}
              </ul>
            )}
            {importResult.report.accountsAdded > 0 && (
              <p
                className="mt-2 text-[11px]"
                style={{ color: "var(--fg-subtle)" }}
              >
                {importResult.report.passwordsRestored === 0
                  ? t("settings.backup.restartHintNoPasswords")
                  : t("settings.backup.restartHint")}
              </p>
            )}
          </div>
        )}
        {importResult?.kind === "err" && (
          <span
            className="text-xs"
            style={{ color: "var(--accent-err, #dc2626)" }}
          >
            {importResult.msg}
          </span>
        )}
      </section>
    </div>
  );
}

/**
 * Default-Filename `crystalmail-backup-YYYY-MM-DD.json`. Datums-suffix
 * macht es trivial mehrere Backups nebeneinander zu halten ohne dass
 * sich der User ständig einen neuen Namen ausdenken muss.
 */
function defaultBackupFilename(): string {
  const now = new Date();
  const yyyy = now.getFullYear();
  const mm = String(now.getMonth() + 1).padStart(2, "0");
  const dd = String(now.getDate()).padStart(2, "0");
  return `crystalmail-backup-${yyyy}-${mm}-${dd}.json`;
}
