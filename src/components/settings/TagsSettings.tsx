import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { Tag } from "../../types";

/** Beliebige Farb-Palette für Tag-Chips. Hand-gepicked weil es sieben
 *  ist — alle in oklch leicht entsättigt damit sie auf weißer wie auch
 *  dunkler UI nicht knallen. NULL = Default (Theme-bg-hover). */
const COLOR_PRESETS: { value: string | null; label: string }[] = [
  { value: null, label: "—" },
  { value: "#3b82f6", label: "Blau" },
  { value: "#10b981", label: "Grün" },
  { value: "#f59e0b", label: "Bernstein" },
  { value: "#ef4444", label: "Rot" },
  { value: "#a855f7", label: "Violett" },
  { value: "#ec4899", label: "Pink" },
  { value: "#64748b", label: "Schiefer" },
];

/**
 * Tags-Verwaltung. Liste aller existierenden Tags, jeder mit Inline-
 * Rename + Color-Picker + Delete. Anlegen erfolgt im ContactDetail-
 * Picker (lazy: ein Tag entsteht beim ersten Verlinken). Hier auf
 * der Settings-Ebene haben wir trotzdem einen "+ Neuer Tag"-Knopf
 * für User die ihr Vokabular vorab strukturieren wollen — gerade
 * sinnvoll bevor der Auto-Extract das erste Mal läuft, damit pi
 * eine Liste hat.
 */
export function TagsSettings() {
  const { t } = useTranslation();
  const [tags, setTags] = useState<Tag[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [newTagName, setNewTagName] = useState("");
  const [creating, setCreating] = useState(false);

  const load = useCallback(async () => {
    try {
      const list = await invoke<Tag[]>("list_tags");
      setTags(list);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const createTag = async () => {
    const name = newTagName.trim();
    if (!name) return;
    setCreating(true);
    setError(null);
    try {
      await invoke<Tag>("upsert_tag", { name, color: null });
      setNewTagName("");
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setCreating(false);
    }
  };

  const updateTag = async (tag: Tag, patch: Partial<Tag>) => {
    setError(null);
    try {
      await invoke("update_tag", { tag: { ...tag, ...patch } });
      await load();
    } catch (e) {
      setError(String(e));
    }
  };

  const deleteTag = async (tag: Tag) => {
    if (!window.confirm(t("settings.tags.confirmDelete", { name: tag.name })))
      return;
    setError(null);
    try {
      await invoke("delete_tag", { tagId: tag.id });
      await load();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="flex flex-col gap-5">
      <header>
        <h2 className="text-base font-semibold">{t("settings.tags.title")}</h2>
        <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.tags.hint")}
        </p>
      </header>

      {error && (
        <div
          className="rounded-md px-3 py-2 text-xs"
          style={{
            background: "rgba(248,113,113,0.12)",
            color: "#ef4444",
          }}
        >
          {error}
        </div>
      )}

      {/* Anlegen — Inline-Form. */}
      <section className="flex items-center gap-2">
        <input
          value={newTagName}
          onChange={(e) => setNewTagName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void createTag();
            }
          }}
          placeholder={t("settings.tags.newPlaceholder")}
          className="flex-1 rounded-md px-2 py-1 text-sm outline-none"
          style={{
            background: "var(--bg-base)",
            color: "var(--fg-base)",
            border: "1px solid var(--border-base)",
          }}
        />
        <button
          type="button"
          onClick={() => void createTag()}
          disabled={creating || !newTagName.trim()}
          className="rounded-md px-3 py-1 text-sm font-medium disabled:opacity-50"
          style={{ background: "var(--accent)", color: "white" }}
        >
          {creating ? t("settings.tags.creating") : t("settings.tags.create")}
        </button>
      </section>

      {/* Liste. */}
      {tags.length === 0 ? (
        <p
          className="rounded-md border px-3 py-2 text-sm"
          style={{
            color: "var(--fg-subtle)",
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
          }}
        >
          {t("settings.tags.empty")}
        </p>
      ) : (
        <ul className="flex flex-col gap-1">
          {tags.map((tag) => (
            <TagRow
              key={tag.id}
              tag={tag}
              onRename={(name) => void updateTag(tag, { name })}
              onColorChange={(color) => void updateTag(tag, { color })}
              onDelete={() => void deleteTag(tag)}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function TagRow({
  tag,
  onRename,
  onColorChange,
  onDelete,
}: {
  tag: Tag;
  onRename: (name: string) => void;
  onColorChange: (color: string | null) => void;
  onDelete: () => void;
}) {
  const { t } = useTranslation();
  // Lokaler Edit-Buffer fürs Rename — wir wollen nicht jeden
  // Tastenanschlag DB-roundtrippen.
  const [name, setName] = useState(tag.name);
  const [colorOpen, setColorOpen] = useState(false);

  // Wenn der Tag von außen aktualisiert wird (anderer Edit-Pfad,
  // re-load), den lokalen Buffer angleichen.
  useEffect(() => {
    setName(tag.name);
  }, [tag.name]);

  const commitRename = () => {
    const trimmed = name.trim();
    if (!trimmed || trimmed === tag.name) {
      setName(tag.name);
      return;
    }
    onRename(trimmed);
  };

  return (
    <li
      className="flex items-center gap-2 rounded-md border px-3 py-1.5"
      style={{
        borderColor: "var(--border-soft)",
        background: "var(--bg-base)",
      }}
    >
      {/* Color-Swatch + Picker */}
      <div className="relative">
        <button
          type="button"
          onClick={() => setColorOpen((v) => !v)}
          aria-label={t("settings.tags.changeColor")}
          className="h-5 w-5 rounded border"
          style={{
            background: tag.color ?? "var(--bg-hover)",
            borderColor: "var(--border-base)",
          }}
        />
        {colorOpen && (
          <div
            className="absolute left-0 top-full z-50 mt-1 grid grid-cols-4 gap-1 rounded-md border p-2 shadow-lg"
            style={{
              background: "var(--bg-panel)",
              borderColor: "var(--border-base)",
            }}
          >
            {COLOR_PRESETS.map((preset) => (
              <button
                key={preset.label}
                type="button"
                onClick={() => {
                  onColorChange(preset.value);
                  setColorOpen(false);
                }}
                title={preset.label}
                className="h-6 w-6 rounded border"
                style={{
                  background: preset.value ?? "var(--bg-hover)",
                  borderColor:
                    tag.color === preset.value
                      ? "var(--accent)"
                      : "var(--border-base)",
                  borderWidth: tag.color === preset.value ? 2 : 1,
                }}
              />
            ))}
          </div>
        )}
      </div>

      {/* Name */}
      <input
        value={name}
        onChange={(e) => setName(e.target.value)}
        onBlur={commitRename}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            (e.target as HTMLInputElement).blur();
          } else if (e.key === "Escape") {
            e.preventDefault();
            setName(tag.name);
            (e.target as HTMLInputElement).blur();
          }
        }}
        className="flex-1 rounded px-2 py-0.5 text-sm outline-none"
        style={{
          background: "transparent",
          color: "var(--fg-base)",
          border: "1px solid transparent",
        }}
        onFocus={(e) => (e.currentTarget.style.borderColor = "var(--border-base)")}
        onBlurCapture={(e) =>
          (e.currentTarget.style.borderColor = "transparent")
        }
      />

      {/* Delete */}
      <button
        type="button"
        onClick={onDelete}
        className="rounded px-2 py-0.5 text-xs"
        style={{ color: "#ef4444" }}
        title={t("settings.tags.delete")}
      >
        ✕
      </button>
    </li>
  );
}
