import sparksRaw from "../data/session-sparks.json";
import { PACK, type PackSession } from "../data/pack";

export const PAGE_SIZE = 25;

export const ALL_SESSIONS: PackSession[] = PACK.sessions;

const withStart = ALL_SESSIONS.filter((s) => s.startedTs != null);
export const FIRST_TS = withStart.length ? Math.min(...withStart.map((s) => s.startedTs as number)) : 0;
export const LAST_TS = (() => {
  const ends = ALL_SESSIONS.map((s) => s.endedTs ?? s.startedTs).filter((n): n is number => n != null);
  return ends.length ? Math.max(...ends) : FIRST_TS;
})();

export const FIRST_AT =
  ALL_SESSIONS.find((s) => s.startedTs === FIRST_TS)?.startedAt ?? "unavailable";
export const LAST_AT =
  ALL_SESSIONS.find((s) => (s.endedTs ?? s.startedTs) === LAST_TS)?.endedAt ??
  ALL_SESSIONS.find((s) => s.startedTs === LAST_TS)?.startedAt ??
  "unavailable";

export const PARENT_IDS = new Set(ALL_SESSIONS.map((s) => s.id));

export const PROVIDERS = [...new Set(ALL_SESSIONS.map((s) => s.provider))].sort();
export const PROJECTS = [...new Set(ALL_SESSIONS.map((s) => s.project))].sort();

export type RangeId = "24h" | "7d" | "30d" | "custom";

export function windowFor(range: RangeId): { t0: number; t1: number } {
  const t1 = LAST_TS;
  if (range === "24h") return { t0: t1 - 86400, t1 };
  if (range === "7d") return { t0: t1 - 86400 * 7, t1 };
  if (range === "30d") return { t0: t1 - 86400 * 30, t1 };
  return { t0: FIRST_TS, t1: LAST_TS };
}

export function inWindow(s: PackSession, t0: number, t1: number) {
  const t = s.startedTs;
  if (t == null) return false;
  return t >= t0 && t <= t1;
}

type SparkFile = {
  rows: Record<string, { v: number[]; uniqueTs: number; kind: string }>;
  points?: [number, number, number][];
};

const SPARK_FILE = sparksRaw as unknown as SparkFile;
const SPARK_IDS = Object.keys(SPARK_FILE.rows);
const SESSION_BY_ID = new Map(ALL_SESSIONS.map((s) => [s.id, s]));

export type SpikeTick = {
  ts: number;
  bucketStart: number;
  bucketEnd: number;
  n: number;
  id: string;
  provider: string;
  messages: number;
};

export type TimeWindow = { t0: number; t1: number };
export type BucketLevel = { seconds: number; label: "DAY" | "HOUR" | "EVENT" };

export function isTimeWindow(value: unknown): value is TimeWindow {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Partial<TimeWindow>;
  return typeof candidate.t0 === "number" && Number.isFinite(candidate.t0) &&
    typeof candidate.t1 === "number" && Number.isFinite(candidate.t1) &&
    candidate.t1 > candidate.t0;
}

export function bucketLevel(span: number): BucketLevel {
  if (span > 86400 * 10) return { seconds: 86400, label: "DAY" };
  if (span > 86400 / 2) return { seconds: 3600, label: "HOUR" };
  return { seconds: 0, label: "EVENT" };
}

export function clampWindow(window: TimeWindow, minSpan = 60): TimeWindow {
  const bounds = Math.max(1, LAST_TS - FIRST_TS);
  const span = Math.min(bounds, Math.max(minSpan, window.t1 - window.t0));
  const t0 = Math.min(LAST_TS - span, Math.max(FIRST_TS, window.t0));
  return { t0, t1: t0 + span };
}

export function zoomWindow(window: TimeWindow, anchor: number, factor: number): TimeWindow {
  const span = Math.max(1, window.t1 - window.t0);
  const ratio = Math.max(0, Math.min(1, (anchor - window.t0) / span));
  const nextSpan = span * factor;
  return clampWindow({ t0: anchor - nextSpan * ratio, t1: anchor + nextSpan * (1 - ratio) });
}

export const SPIKES: SpikeTick[] = (() => {
  const pts = SPARK_FILE.points;
  if (pts?.length) {
    const out: SpikeTick[] = [];
    for (const [ts, n, i] of pts) {
      const id = SPARK_IDS[i];
      const s = id ? SESSION_BY_ID.get(id) : undefined;
      if (!s) continue;
      out.push({ ts, bucketStart: ts, bucketEnd: ts, n, id, provider: s.provider, messages: s.messages });
    }
    return out;
  }
  return ALL_SESSIONS.filter((s) => s.startedTs != null).map((s) => ({
    ts: s.startedTs as number,
    bucketStart: s.startedTs as number,
    bucketEnd: s.startedTs as number,
    n: Math.max(1, s.messages),
    id: s.id,
    provider: s.provider,
    messages: s.messages,
  }));
})();

/**
 * Message volume per session at the requested semantic bucket size.
 * One needle per session per active bucket, positioned at the volume-weighted
 * mean of that session's real timestamps inside the bucket — so simultaneous
 * sessions fan out into distinct needles. Empty hours produce nothing;
 * needles are never invented.
 */
export function volumeInWindow(sessions: PackSession[], t0: number, t1: number, bucketSeconds = 3600): SpikeTick[] {
  const allow = new Set(sessions.map((s) => s.id));
  if (bucketSeconds <= 0) {
    return SPIKES.filter((tick) => allow.has(tick.id) && tick.ts >= t0 && tick.ts <= t1);
  }
  const buckets = new Map<string, { start: number; wts: number; provider: string; n: number; id: string }>();
  for (const t of SPIKES) {
    if (!allow.has(t.id) || t.ts < t0 || t.ts > t1) continue;
    const key = `${Math.floor(t.ts / bucketSeconds)}:${t.id}`;
    let b = buckets.get(key);
    if (!b) {
      b = { start: Math.floor(t.ts / bucketSeconds) * bucketSeconds, wts: 0, provider: t.provider, n: 0, id: t.id };
      buckets.set(key, b);
    }
    b.n += t.n;
    b.wts += t.ts * t.n;
  }
  const out: SpikeTick[] = [];
  for (const b of buckets.values()) {
    out.push({
      ts: b.wts / b.n,
      bucketStart: b.start,
      bucketEnd: b.start + bucketSeconds - 1,
      n: b.n,
      id: b.id,
      provider: b.provider,
      messages: b.n,
    });
  }
  return out.sort((a, b) => a.ts - b.ts);
}

const MON = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

export function fmtDay(ts: number) {
  const d = new Date(ts * 1000);
  return `${String(d.getUTCDate()).padStart(2, "0")} ${MON[d.getUTCMonth()]}`;
}

export function fmtDayYear(ts: number) {
  const d = new Date(ts * 1000);
  return `${d.getUTCDate()} ${MON[d.getUTCMonth()]} ${d.getUTCFullYear()}`;
}

export function stripUtc(at: string | null) {
  if (!at) return "—";
  return at.replace(/ UTC$/, "").replace(/(\d{2}:\d{2}):\d{2}$/, "$1");
}

/** Date and HH:MM as separate tokens so START/END can wrap without clipping minutes. */
export function spanParts(at: string | null): { date: string; time: string } {
  const s = stripUtc(at);
  if (!s || s === "—") return { date: "—", time: "" };
  const i = s.lastIndexOf(" ");
  if (i < 0) return { date: s, time: "" };
  return { date: s.slice(0, i), time: s.slice(i + 1) };
}

/** 0..1 height fraction on a log scale so low counts stay visible as needles. */
export function logFrac(n: number, max: number) {
  if (max <= 0) return 0;
  return Math.log1p(Math.max(0, n)) / Math.log1p(max);
}

function fmtCount(n: number) {
  if (n >= 1000) return `${Math.round(n / 100) / 10}K`.replace(".0K", "K");
  return String(n);
}

/** Evenly spaced axis labels for the log height scale, top → bottom. */
export function logAxisLabels(max: number, steps = 7): string[] {
  const out: string[] = [];
  for (let i = 0; i < steps; i++) {
    const f = (steps - 1 - i) / (steps - 1);
    const v = Math.round(Math.expm1(f * Math.log1p(max)));
    out.push(fmtCount(v));
  }
  return out;
}

export function niceMax(n: number) {
  if (n <= 10) return 10;
  if (n <= 25) return 25;
  if (n <= 50) return 50;
  if (n <= 100) return 100;
  if (n <= 250) return 250;
  if (n <= 500) return 500;
  if (n <= 1000) return 1000;
  if (n <= 2000) return 2000;
  if (n <= 4000) return 4000;
  if (n <= 8000) return 8000;
  return Math.ceil(n / 1000) * 1000;
}

export function recency(u: number) {
  const t = Math.max(0, Math.min(1, u));
  const r = Math.round(240 + (103 - 240) * t);
  const g = Math.round(180 + (232 - 180) * t);
  const b = Math.round(41 + (249 - 41) * t);
  return `rgb(${r},${g},${b})`;
}

/** Provider hue with a slight recency lift. Claude cyan, cursor amber, else gold. */
export function spikeColor(provider: string, u: number): string {
  const t = Math.max(0, Math.min(1, u));
  if (provider === "cursor") {
    const r = Math.round(228 + (255 - 228) * t);
    const g = Math.round(162 + (204 - 162) * t);
    const b = Math.round(28 + (72 - 28) * t);
    return `rgb(${r},${g},${b})`;
  }
  if (provider === "codex") {
    const r = Math.round(210 + (240 - 210) * t);
    const g = Math.round(186 + (214 - 186) * t);
    const b = Math.round(72 + (110 - 72) * t);
    return `rgb(${r},${g},${b})`;
  }
  const r = Math.round(64 + (120 - 64) * t);
  const g = Math.round(196 + (236 - 196) * t);
  const b = Math.round(216 + (252 - 216) * t);
  return `rgb(${r},${g},${b})`;
}

export const COLUMNS = [
  { id: "id", lab: "SESSION ID" },
  { id: "provider", lab: "PROVIDER" },
  { id: "project", lab: "PROJECT" },
  { id: "span", lab: "START / END" },
  { id: "messages", lab: "MESSAGES" },
  { id: "tokens", lab: "TOKENS (PROVENANCE)" },
  { id: "coverage", lab: "COVERAGE" },
  { id: "status", lab: "STATUS" },
] as const;

export type ColId = (typeof COLUMNS)[number]["id"];

export type SortDir = "asc" | "desc";

const SORT_KEY: Record<ColId, (s: PackSession) => string | number> = {
  id: (s) => s.id,
  provider: (s) => s.provider,
  project: (s) => s.project,
  span: (s) => s.startedTs ?? 0,
  messages: (s) => s.messages,
  // Tokens were not copied into this snapshot: every row sorts equal (stable).
  tokens: () => 0,
  coverage: (s) => s.coverage,
  status: (s) => s.status,
};

export function sortSessions(list: PackSession[], col: ColId | null, dir: SortDir): PackSession[] {
  if (!col) return list;
  const key = SORT_KEY[col];
  const sign = dir === "asc" ? 1 : -1;
  return [...list].sort((a, b) => {
    const ka = key(a);
    const kb = key(b);
    if (ka < kb) return -1 * sign;
    if (ka > kb) return 1 * sign;
    return 0;
  });
}
