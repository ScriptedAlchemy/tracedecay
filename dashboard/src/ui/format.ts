/** Compact numeric language for brain-scale magnitudes (12.8M nodes, 932k
 * edges): tabular-friendly, one decimal only when it carries information. */
export function formatCount(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return '—';
  const abs = Math.abs(value);
  if (abs >= 1_000_000_000) return `${trim(value / 1_000_000_000)}B`;
  if (abs >= 1_000_000) return `${trim(value / 1_000_000)}M`;
  if (abs >= 10_000) return `${trim(value / 1_000)}k`;
  return value.toLocaleString();
}

function trim(value: number): string {
  const rounded = Math.round(value * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

/** The same magnitude language, split so the instrument can set the unit small
 * and quiet beside the number. `null` in, em dash out — never a zero. */
export function splitCount(value: number | null | undefined): {
  value: string;
  unit?: string;
} {
  if (value == null || !Number.isFinite(value)) return { value: '—' };
  const abs = Math.abs(value);
  if (abs >= 1_000_000_000) return { value: trim(value / 1_000_000_000), unit: 'B' };
  if (abs >= 1_000_000) return { value: trim(value / 1_000_000), unit: 'M' };
  if (abs >= 10_000) return { value: trim(value / 1_000), unit: 'K' };
  return { value: value.toLocaleString() };
}

/** A wall-clock stamp trimmed to what a dense row can actually carry.
 *
 * `toLocaleString()` prints "7/24/2026, 9:07:20 PM" — twenty-one glyphs of
 * which the seconds are noise in a list you scan for ordering, and the width
 * forces the row's real content to truncate. This keeps the full calendar date
 * and the minute and drops only the seconds, in a fixed-width ISO-ish form
 * that sorts and aligns as a column. Absolute, not relative: a screenshot of a
 * fixture has to render identically tomorrow.
 *
 * The unabbreviated value stays available — every row that uses this also
 * exposes the raw record in its inspector. */
export function formatStamp(epochSeconds: number | null | undefined): string {
  if (epochSeconds == null || !Number.isFinite(epochSeconds)) return '—';
  const date = new Date(epochSeconds * 1000);
  if (Number.isNaN(date.getTime())) return '—';
  const pad = (n: number) => String(n).padStart(2, '0');
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}` +
    ` ${pad(date.getHours())}:${pad(date.getMinutes())}`
  );
}

/** Byte magnitudes with the unit split out for the same reason. */
export function splitBytes(bytes: number | null | undefined): {
  value: string;
  unit?: string;
} {
  if (bytes == null || !Number.isFinite(bytes)) return { value: '—' };
  if (bytes >= 1024 ** 3) return { value: (bytes / 1024 ** 3).toFixed(2), unit: 'GiB' };
  if (bytes >= 1024 ** 2) return { value: (bytes / 1024 ** 2).toFixed(1), unit: 'MiB' };
  if (bytes >= 1024) return { value: (bytes / 1024).toFixed(1), unit: 'KiB' };
  return { value: String(bytes), unit: 'B' };
}
