/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Pure (non-React) helpers for the hermes-lcm dashboard plugin.
 *
 * Ported 1:1 from the original hand-written IIFE in `index.js`; behavior is
 * intentionally identical. These helpers are split out only for navigability —
 * the IIFE surfaced them as closed-over module locals.
 */

const SDK: any =
  (typeof window !== "undefined" && (window as any).__HERMES_PLUGIN_SDK__) || {};

/** Relative-time formatter exposed by the host SDK (Hermes or standalone shell).
 *  Null when the host doesn't provide one — `fmtTime` falls back to toLocaleString. */
export const isoTimeAgo: ((iso: string) => string) | null =
  (SDK.utils && SDK.utils.isoTimeAgo) || null;

export const API = "/api/plugins/hermes-lcm";

export const SEARCH_FETCH_LIMIT = 120;
export const SEARCH_PAGE_SIZE = 6;
export const SESSION_FETCH_BATCH = 60;
export const SESSION_MESSAGE_PAGE_SIZE = 8;

export function short(s: any, n: number): string {
  const text = String(s || "");
  return text.length > n ? text.slice(0, n - 1) + "…" : text;
}

export function fmtInt(n: any): string {
  const v = Number(n) || 0;
  return v.toLocaleString();
}

export function fmtTime(epoch: any): string {
  const v = Number(epoch);
  if (!v) return "";
  try {
    const d = new Date(v * 1000);
    if (isoTimeAgo) return isoTimeAgo(d.toISOString());
    return d.toLocaleString();
  } catch (e) {
    return String(epoch);
  }
}

export function fmtAbsoluteTime(epoch: any): string {
  const v = Number(epoch);
  if (!v) return "";
  try {
    return new Date(v * 1000).toLocaleString();
  } catch (e) {
    return String(epoch);
  }
}

export function escapeRegExp(s: any): string {
  return String(s || "").replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function queryTerms(query: any): string[] {
  return String(query || "")
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 8);
}

export function parseJsonArray(value: any): any[] {
  if (Array.isArray(value)) return value;
  if (typeof value !== "string" || !value.trim()) return [];
  try {
    const parsed = JSON.parse(value);
    return Array.isArray(parsed) ? parsed : [];
  } catch (e) {
    return [];
  }
}

export function copyTextValue(text: any): Promise<boolean> {
  const value = String(text == null ? "" : text);
  if (!value) return Promise.resolve(false);
  if (typeof navigator !== "undefined" && navigator.clipboard && navigator.clipboard.writeText) {
    return navigator.clipboard.writeText(value).then(function () { return true; }).catch(function () {
      return false;
    });
  }
  return new Promise(function (resolve) {
    try {
      const ta = document.createElement("textarea");
      ta.value = value;
      ta.setAttribute("readonly", "readonly");
      ta.style.position = "absolute";
      ta.style.left = "-9999px";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      document.body.removeChild(ta);
      resolve(ok);
    } catch (e) {
      resolve(false);
    }
  });
}

/** Humanize fetch failures: the shell SDK's fetchJSON throws (TypeError
 *  "Failed to fetch") when the server is unreachable. Failed fetches must
 *  render error UI, never zero-data UI, so this string is shown prominently. */
export function friendlyError(err: any): string {
  const raw = String((err && err.message) || err || "Request failed");
  if (/failed to fetch|networkerror|load failed|network request/i.test(raw)) {
    return "Can't reach the tracedecay server";
  }
  return raw;
}

/** Append rows from a paginated follow-up fetch, de-duplicated by id, so
 *  server-offset pagination can never double-render a row. */
export function mergeRows(prevRows: any, nextRows: any, idKey: string): any[] {
  const seen: Record<string, boolean> = {};
  const merged = (prevRows || []).slice();
  merged.forEach(function (row) { seen[String(row[idKey])] = true; });
  (nextRows || []).forEach(function (row) {
    const key = String(row[idKey]);
    if (!seen[key]) {
      seen[key] = true;
      merged.push(row);
    }
  });
  return merged;
}

/** Merge a follow-up search page into the previous payload: scalar fields
 *  (totals, engine, …) come from the newest response, match rows append with
 *  id-dedupe. Pure so the pagination merge stays unit-testable. */
export function mergeSearchPayload(prev: any, json: any): any {
  if (!prev) return json;
  const prevMatches = prev.matches || {};
  const nextMatches = (json && json.matches) || {};
  return Object.assign({}, prev, json, {
    matches: {
      messages: mergeRows(prevMatches.messages, nextMatches.messages, "store_id"),
      summary_nodes: mergeRows(prevMatches.summary_nodes, nextMatches.summary_nodes, "node_id"),
    },
  });
}

/** Flatten markdown to readable plain text (for compact list previews/titles). */
export function stripMd(s: any): string {
  return String(s == null ? "" : s)
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/^\s*>\s?/gm, "")
    .replace(/^\s*[-*+]\s+/gm, "")
    .replace(/\[([^\]]+)\]\([^)\s]+\)/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

/** Derive a short title from a summary: first heading, else first bold run,
 *  else the first sentence/line of the flattened text. */
export function summaryTitle(s: any): string {
  const txt = String(s == null ? "" : s);
  const hd = txt.match(/^\s*#{1,6}\s+(.+?)\s*$/m);
  if (hd) return stripMd(hd[1]);
  const bold = txt.match(/\*\*([^*]+)\*\*/);
  if (bold) return stripMd(bold[1]);
  const flat = stripMd(txt);
  const dot = flat.search(/[.!?](\s|$)/);
  return dot > 12 && dot < 90 ? flat.slice(0, dot + 1) : flat;
}

/** Pretty session label from an id like "20260529_011608_ab12cd". */
export function sessionLabel(id: any): string {
  const txt = String(id == null ? "" : id);
  const m = txt.match(/^(\d{4})(\d{2})(\d{2})_(\d{2})(\d{2})(\d{2})/);
  if (m) return `${m[1]}-${m[2]}-${m[3]} ${m[4]}:${m[5]}`;
  return short(txt, 36);
}

export function sessionTail(id: any): string {
  const txt = String(id == null ? "" : id);
  const m = txt.match(/_([0-9a-f]{4,})$/i);
  return m ? m[1] : "";
}

/** Tool results are often a JSON value followed by a trailing human note
 *  (e.g. `{...}\n[use offset=120 to see more]`), so strict JSON.parse fails.
 *  Extract the leading {...}/[...] by brace-matching, keep the rest as a note. */
export function parseLeadingJSON(s: any): { value: any; rest: string } | null {
  if (typeof s !== "string") return null;
  let i = 0;
  while (i < s.length && /\s/.test(s[i])) i++;
  const open = s[i];
  if (open !== "{" && open !== "[") {
    try { return { value: JSON.parse(s), rest: "" }; } catch (e) { return null; }
  }
  const close = open === "{" ? "}" : "]";
  let depth = 0, inStr = false, esc = false, end = -1;
  for (let j = i; j < s.length; j++) {
    const ch = s[j];
    if (inStr) {
      if (esc) esc = false;
      else if (ch === "\\") esc = true;
      else if (ch === '"') inStr = false;
      continue;
    }
    if (ch === '"') inStr = true;
    else if (ch === open) depth++;
    else if (ch === close) { depth--; if (depth === 0) { end = j + 1; break; } }
  }
  if (end === -1) return null;
  try { return { value: JSON.parse(s.slice(i, end)), rest: s.slice(end).trim() }; }
  catch (e) { return null; }
}

export function clampText(s: any, n: number): string {
  const t = String(s == null ? "" : s);
  return t.length > n ? t.slice(0, n) + "\n…(" + fmtInt(t.length - n) + " more chars)" : t;
}

export function ratioStr(src: any, out: any): string {
  const s = Number(src) || 0;
  const o = Number(out) || 0;
  if (!o) return "—";
  return (Math.round((s / o) * 10) / 10) + "×";
}
