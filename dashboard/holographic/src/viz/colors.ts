/**
 * Shared categorical color assignment for the holographic charts.
 *
 * Colors are CSS custom properties (`--hm-cat-N`, defined in styles.css with
 * dark defaults and light-theme overrides), so every chart re-themes
 * automatically when the shell toggles `data-theme`.
 */

export const CATEGORY_COLOR_COUNT = 8;

/** CSS color expression for categorical slot `i`. */
export function slotColor(i: number): string {
  return `var(--hm-cat-${((i % CATEGORY_COLOR_COUNT) + CATEGORY_COLOR_COUNT) % CATEGORY_COLOR_COUNT})`;
}

/**
 * Stable category→color map: categories are sorted so the same category gets
 * the same color across the map, legend, overview bars, and similarity badges
 * regardless of data order.
 */
export function categoryColorMap(categories: string[]): Map<string, string> {
  const unique = Array.from(new Set(categories)).sort((a, b) => a.localeCompare(b));
  const map = new Map<string, string>();
  unique.forEach((cat, i) => map.set(cat, slotColor(i)));
  return map;
}

/** Translucent variant of a palette color (for fills behind strokes). */
export function withAlpha(color: string, alphaPct: number): string {
  return `color-mix(in srgb, ${color} ${alphaPct}%, transparent)`;
}
