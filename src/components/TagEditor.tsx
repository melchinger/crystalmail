import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { Tag } from "../types";

type Props = {
  /** Aktuell gesetzte Tags des Kontakts. */
  tags: Tag[];
  /** Wird mit der NEUEN Tag-Liste aufgerufen — Caller persistiert. */
  onChange: (tagIds: string[]) => Promise<void> | void;
};

/** Inline-Tag-Editor für ContactDetail. Chips zeigen aktive Tags mit
 *  ✕-Remove, "+"-Button öffnet einen kleinen Picker mit Suchfeld +
 *  Liste aller existierenden Tags + Option "neu anlegen unter diesem
 *  Namen" wenn das Suchwort kein existierendes Tag matcht. */
export function TagEditor({ tags, onChange }: Props) {
  const { t } = useTranslation();
  const [pickerOpen, setPickerOpen] = useState(false);
  const [allTags, setAllTags] = useState<Tag[]>([]);
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const loadTags = async () => {
    try {
      const list = await invoke<Tag[]>("list_tags");
      setAllTags(list);
    } catch (e) {
      console.warn("list_tags failed:", e);
    }
  };

  useEffect(() => {
    if (pickerOpen) {
      void loadTags();
      // Focus aufs Suchfeld nach dem Open — nächster Tick damit der
      // Render durch ist.
      window.setTimeout(() => inputRef.current?.focus(), 0);
    }
  }, [pickerOpen]);

  const activeIds = new Set(tags.map((t) => t.id));
  const filterLower = filter.trim().toLowerCase();
  const filtered = allTags.filter((t) =>
    filterLower.length === 0 ? true : t.name.toLowerCase().includes(filterLower),
  );
  const exactMatch = allTags.find(
    (t) => t.name.toLowerCase() === filterLower && filterLower.length > 0,
  );
  const showCreate =
    filterLower.length > 0 && !exactMatch && !activeIds.has(filter);

  const addTagById = async (tagId: string) => {
    if (activeIds.has(tagId)) return;
    setBusy(true);
    try {
      await onChange([...tags.map((t) => t.id), tagId]);
    } finally {
      setBusy(false);
    }
  };

  const removeTagById = async (tagId: string) => {
    setBusy(true);
    try {
      await onChange(tags.filter((t) => t.id !== tagId).map((t) => t.id));
    } finally {
      setBusy(false);
    }
  };

  const createAndAdd = async () => {
    const name = filter.trim();
    if (!name) return;
    setBusy(true);
    try {
      const created = await invoke<Tag>("upsert_tag", { name, color: null });
      // Lokale Tag-Liste und aktive Tags aktualisieren in einem Rutsch.
      setAllTags((prev) =>
        prev.some((t) => t.id === created.id) ? prev : [...prev, created],
      );
      await onChange([...tags.map((t) => t.id), created.id]);
      setFilter("");
    } catch (e) {
      console.warn("upsert_tag failed:", e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex flex-wrap items-center gap-1.5">
      {tags.map((tag) => (
        <span
          key={tag.id}
          className="inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px]"
          style={{
            background: tag.color ?? "var(--bg-hover)",
            borderColor: "var(--border-base)",
            color: tag.color ? "#fff" : "var(--fg-base)",
          }}
        >
          <span>{tag.name}</span>
          <button
            type="button"
            onClick={() => void removeTagById(tag.id)}
            disabled={busy}
            className="opacity-70 hover:opacity-100 disabled:opacity-30"
            aria-label={t("tags.remove")}
            style={{ color: "inherit" }}
          >
            ✕
          </button>
        </span>
      ))}
      <div className="relative">
        <button
          type="button"
          onClick={() => setPickerOpen((v) => !v)}
          disabled={busy}
          className="rounded-full border px-2 py-0.5 text-[11px] disabled:opacity-50"
          style={{
            borderColor: "var(--border-base)",
            color: "var(--fg-muted)",
            background: "transparent",
          }}
        >
          + {t("tags.add")}
        </button>
        {pickerOpen && (
          <div
            className="absolute left-0 top-full z-50 mt-1 max-h-[260px] w-[240px] overflow-y-auto rounded-md border shadow-lg"
            style={{
              background: "var(--bg-panel)",
              borderColor: "var(--border-base)",
            }}
          >
            <input
              ref={inputRef}
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  e.preventDefault();
                  setPickerOpen(false);
                  setFilter("");
                } else if (e.key === "Enter") {
                  e.preventDefault();
                  if (showCreate) void createAndAdd();
                  else if (filtered.length > 0)
                    void addTagById(filtered[0].id);
                }
              }}
              placeholder={t("tags.searchOrCreate")}
              className="w-full border-b px-2 py-1 text-sm outline-none"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
                color: "var(--fg-base)",
              }}
            />
            <ul>
              {filtered.length === 0 && !showCreate && (
                <li
                  className="px-2 py-1 text-xs"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {t("tags.empty")}
                </li>
              )}
              {filtered.map((tg) => {
                const checked = activeIds.has(tg.id);
                return (
                  <li
                    key={tg.id}
                    onClick={() => {
                      if (checked) void removeTagById(tg.id);
                      else void addTagById(tg.id);
                    }}
                    className="flex cursor-pointer items-center gap-2 px-2 py-1 text-sm"
                    onMouseEnter={(e) =>
                      (e.currentTarget.style.background = "var(--bg-hover)")
                    }
                    onMouseLeave={(e) =>
                      (e.currentTarget.style.background = "transparent")
                    }
                  >
                    <span
                      className="inline-block h-3 w-3 rounded-sm border"
                      style={{
                        background: checked ? "var(--accent)" : "transparent",
                        borderColor: "var(--border-base)",
                      }}
                    >
                      {checked && (
                        <span
                          aria-hidden
                          style={{ color: "white", fontSize: "10px" }}
                        >
                          ✓
                        </span>
                      )}
                    </span>
                    <span style={{ color: "var(--fg-base)" }}>{tg.name}</span>
                  </li>
                );
              })}
              {showCreate && (
                <li
                  onClick={() => void createAndAdd()}
                  className="cursor-pointer border-t px-2 py-1 text-sm"
                  style={{
                    borderColor: "var(--border-soft)",
                    color: "var(--accent)",
                  }}
                  onMouseEnter={(e) =>
                    (e.currentTarget.style.background = "var(--bg-hover)")
                  }
                  onMouseLeave={(e) =>
                    (e.currentTarget.style.background = "transparent")
                  }
                >
                  + {t("tags.createNew", { name: filter.trim() })}
                </li>
              )}
            </ul>
          </div>
        )}
      </div>
    </div>
  );
}
