/**
 * Pure display/aggregation helpers for the Savings & Cost tab.
 * No React/SDK imports — unit-tested by `dashboard/test/savings-logic.test.mjs`.
 */

/** 1234 → "1,234", 53_200 → "53.2k", 47_400_000 → "47.4M". */
export function fmtTokens(value: number | null | undefined): string {
  const n = Number(value || 0);
  if (!Number.isFinite(n)) return "0";
  const abs = Math.abs(n);
  if (abs >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
  if (abs >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (abs >= 10_000) return `${(n / 1_000).toFixed(1)}k`;
  return n.toLocaleString("en-US");
}

/** Adaptive-precision USD: $1,234.56 / $12.34 / $0.0123 / $0.000123 / $0. */
export function fmtUsd(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return "—";
  const n = Number(value);
  if (n === 0) return "$0";
  const abs = Math.abs(n);
  if (abs >= 100) return `$${n.toLocaleString("en-US", { maximumFractionDigits: 0 })}`;
  if (abs >= 1) return `$${n.toFixed(2)}`;
  if (abs >= 0.01) return `$${n.toFixed(3)}`;
  if (abs >= 0.001) return `$${n.toFixed(4)}`;
  return `$${n.toFixed(6)}`;
}

/** Integer percentage share, safe for zero totals. */
export function sharePct(part: number, total: number): number {
  if (!total || total <= 0) return 0;
  return Math.round((part / total) * 100);
}

/**
 * Session titles in the store are often raw prompts that open with
 * `<timestamp>…</timestamp>` / `<user_query>` wrappers — strip tags,
 * collapse whitespace, and clip for table display.
 */
export function cleanTitle(raw: string | null | undefined, maxLength = 90): string {
  const text = String(raw || "")
    .replace(/<[^>]*>/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (!text) return "(untitled session)";
  if (text.length <= maxLength) return text;
  return `${text.slice(0, Math.max(1, maxLength - 1)).trimEnd()}…`;
}

/** Unix-day buckets → contiguous ascending series (gaps filled with zeros). */
export function fillDailySeries<T extends { day: number }>(
  rows: T[],
  value: (row: T) => number,
): Array<{ day: number; value: number }> {
  if (!rows.length) return [];
  const byDay = new Map<number, number>();
  for (const row of rows) {
    byDay.set(row.day, (byDay.get(row.day) || 0) + value(row));
  }
  const days = [...byDay.keys()].sort((a, b) => a - b);
  const out: Array<{ day: number; value: number }> = [];
  const first = days[0];
  const last = days[days.length - 1];
  // Cap the fill at a year so one bogus epoch-0 timestamp can't explode the chart.
  if ((last - first) / 86_400 > 366) {
    return days.map((day) => ({ day, value: byDay.get(day) || 0 }));
  }
  for (let day = first; day <= last; day += 86_400) {
    out.push({ day, value: byDay.get(day) || 0 });
  }
  return out;
}

/** Unix day → "Jun 11" (UTC, matching the ledger's UTC bucketing). */
export function fmtDay(day: number): string {
  const date = new Date(day * 1000);
  const months = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
  ];
  return `${months[date.getUTCMonth()]} ${date.getUTCDate()}`;
}

/** Short project label: last path segment (keeps tmp dirs readable). */
export function projectLabel(path: string): string {
  const segments = String(path || "").split("/").filter(Boolean);
  return segments[segments.length - 1] || path || "(unknown)";
}

export type CostBasis = "actual" | "estimated" | "mixed";

export const BASIS_LABELS: Record<CostBasis, string> = {
  actual: "actual (from transcript usage)",
  estimated: "estimated (computed)",
  mixed: "mixed (usage + estimates)",
};
