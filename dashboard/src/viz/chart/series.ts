/**
 * The non-colour half of a multi-series chart's identity.
 *
 * `Chart` assigns its token palette by series index. These two tables give a
 * legend the same index → mark mapping so the swatch beside a name is drawn
 * in the same register as the line, and give every series a line style as
 * well as a hue, colour is never the sole carrier of which provider a line
 * is, so the styles cycle independently of the palette and the two together
 * stay distinct well past six series.
 */

/** Face-plane utilities in the same order as the chart palette. */
export const SERIES_SWATCH_CLASSES = [
  'bg-accent',
  'bg-alert',
  'bg-state-ready',
  'bg-state-locked',
  'bg-sev-info',
  'bg-text-secondary',
] as const;

export type SeriesLineStyle = 'solid' | 'dashed' | 'dotted';

export const SERIES_LINE_STYLES: readonly SeriesLineStyle[] = ['solid', 'dashed', 'dotted'];

export function seriesSwatchClass(index: number): string {
  return SERIES_SWATCH_CLASSES[index % SERIES_SWATCH_CLASSES.length] ?? 'bg-accent';
}

export function seriesLineStyle(index: number): SeriesLineStyle {
  return SERIES_LINE_STYLES[index % SERIES_LINE_STYLES.length] ?? 'solid';
}
