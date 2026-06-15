/** Locale-grouped integer ("12,345"); null/undefined render as "0". */
export function fmt(n: number | undefined) {
  return Number(n || 0).toLocaleString();
}

/** Clip text to `max` characters with a trailing ellipsis. */
export function short(text: string | null | undefined, max = 72) {
  const raw = String(text || "");
  return raw.length > max ? `${raw.slice(0, max - 1)}…` : raw;
}
