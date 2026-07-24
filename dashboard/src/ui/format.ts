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
