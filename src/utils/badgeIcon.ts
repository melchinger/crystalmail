/**
 * Renders the Windows taskbar overlay icon — a 16×16 PNG-equivalent
 * RGBA buffer with a red circle and the unread count in white. Same
 * shape pCloud / Teams / Outlook draw.
 *
 * Why 16×16: that's what Windows takes for `ITaskbarList3::
 * SetOverlayIcon` at 96 DPI. The OS scales up for hi-DPI displays
 * internally.
 *
 * Font sizing heuristic: single digit at 13 px, two digits at 11 px,
 * three or more shown as "99+" at 9 px. Keeps the glyphs readable at
 * 16 px without bleeding over the circle edge.
 *
 * Return value is a `Uint8Array` of RGBA bytes that the Tauri
 * `set_unread_badge` command wraps in a `tauri::image::Image` and
 * hands to `set_overlay_icon`.
 */
export function renderBadgeRgba(count: number): Uint8Array | null {
  if (count <= 0) return null;

  const size = 16;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;

  // Glowing dark-purple disc. Radial gradient from a bright violet
  // core (violet-400) through violet-600 to a deep violet-900 edge,
  // which reads as a small light source at 16 px without needing an
  // outer halo (no room for one inside the tile). Slight inset so
  // the anti-aliased outer edge stays inside the 16-px box — Windows
  // will clip pixels that ride the transparent border.
  const grad = ctx.createRadialGradient(
    size / 2,
    size / 2,
    1,
    size / 2,
    size / 2,
    size / 2,
  );
  // Darker purple overall so white glyphs keep contrast. Still a
  // gradient so the badge has depth rather than looking painted-on,
  // but the centre is now a muted violet-700 (not the near-lavender
  // of before) where the digits sit.
  grad.addColorStop(0, "#6d28d9"); // violet-700 core
  grad.addColorStop(0.55, "#4c1d95"); // violet-900
  grad.addColorStop(1, "#2e1065"); // violet-950 edge
  ctx.fillStyle = grad;
  ctx.beginPath();
  ctx.arc(size / 2, size / 2, size / 2 - 0.5, 0, Math.PI * 2);
  ctx.fill();

  // Glyph. Tight: use the system default sans-serif so we don't have
  // to ship a font. Color white for the strongest contrast against
  // the red circle; bold to stay legible even at 9 px.
  let text: string;
  let fontSize: number;
  if (count >= 100) {
    text = "99+";
    fontSize = 8;
  } else if (count >= 10) {
    text = String(count);
    fontSize = 10;
  } else {
    text = String(count);
    fontSize = 12;
  }
  ctx.fillStyle = "#ffffff";
  ctx.font = `bold ${fontSize}px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  // `middle` puts the cap-height centreline at y; nudge down 0.5 px
  // so descender-free glyphs (digits, +) sit visually centred.
  ctx.fillText(text, size / 2, size / 2 + 0.5);

  const img = ctx.getImageData(0, 0, size, size);
  return new Uint8Array(img.data.buffer);
}
