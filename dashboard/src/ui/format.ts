/** Compact numeric language for brain-scale magnitudes (12.8M nodes, 932k
 * edges): tabular-friendly, one decimal only when it carries information.
 *
 * `thousandsAt` is the magnitude at which `k` takes over from the full number.
 * It defaults to 10,000 because a four-figure count still reads exactly and
 * "9,842" beats "9.8k" in a column; a ledger whose small end is already four
 * figures — token counts — abbreviates from 1,000 instead. */
export function formatCount(value: number | null | undefined, thousandsAt = 10_000): string {
  if (value == null || !Number.isFinite(value)) return '—';
  const abs = Math.abs(value);
  if (abs >= 1_000_000_000) return `${trim(value / 1_000_000_000)}B`;
  if (abs >= 1_000_000) return `${trim(value / 1_000_000)}M`;
  if (abs >= thousandsAt) return `${trim(value / 1_000)}k`;
  return value.toLocaleString();
}

function trim(value: number): string {
  const rounded = Math.round(value * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

/** The same magnitude language, split so the instrument can set the unit small
 * and quiet beside the number. `null` in, em dash out — never a zero.
 * `thousandsAt` means what it does in `formatCount`. */
export function splitCount(
  value: number | null | undefined,
  thousandsAt = 10_000,
): {
  value: string;
  unit?: string;
} {
  if (value == null || !Number.isFinite(value)) return { value: '—' };
  const abs = Math.abs(value);
  if (abs >= 1_000_000_000) return { value: trim(value / 1_000_000_000), unit: 'B' };
  if (abs >= 1_000_000) return { value: trim(value / 1_000_000), unit: 'M' };
  if (abs >= thousandsAt) return { value: trim(value / 1_000), unit: 'K' };
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

/** Elide a path from its FRONT, keeping whole segments.
 *
 * A dense row truncates the tail by default, which throws away the only part
 * of a path anyone reads: "dashboard/src/workspac…" identifies nothing, while
 * "…/workspaces/code/CodePage.tsx" identifies the file exactly. The leading
 * ellipsis marks the elision, and callers keep the untruncated string on the
 * element's `title` so nothing is actually lost. */
export function elideStart(path: string | null | undefined, max = 34): string {
  if (!path) return '';
  if (path.length <= max) return path;
  const segments = path.split('/');
  let kept = segments[segments.length - 1] ?? path;
  for (let i = segments.length - 2; i >= 0; i -= 1) {
    const next = `${segments[i]}/${kept}`;
    if (next.length + 2 > max) break;
    kept = next;
  }
  return `…/${kept}`;
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

/** A microsecond wire stamp as an absolute UTC instant.
 *
 * The sentinels are per-call and explicit because they are not interchangeable
 * readings. A horizon `since_micros` of 0 means the projector was asked for an
 * open-ended window, not for 1970 (`zeroAs: 'unbounded'`); a null stamp means
 * the producer never recorded one (`nullAs: 'not reported'`). A site that
 * declares neither has no sentinel case on the wire, and a 0 there is a real
 * epoch instant. */
export function formatMicrosUtc(
  micros: number | null | undefined,
  sentinels: { zeroAs?: string; nullAs?: string } = {},
): string {
  if (micros == null) return sentinels.nullAs ?? '—';
  if (micros === 0 && sentinels.zeroAs != null) return sentinels.zeroAs;
  return new Date(Math.floor(micros / 1000)).toISOString();
}

/** `splitBytes` for a figure whose direction is part of the reading: a store
 * that shrank has to read as a shrink, and the magnitude language must not fork
 * to say so. The sign rides on the value so the unit stays a bare unit. */
export function splitSignedBytes(bytes: number | null | undefined): {
  value: string;
  unit?: string;
} {
  if (bytes == null || !Number.isFinite(bytes)) return { value: '—' };
  const magnitude = splitBytes(Math.abs(bytes));
  return bytes < 0 ? { ...magnitude, value: `-${magnitude.value}` } : magnitude;
}
