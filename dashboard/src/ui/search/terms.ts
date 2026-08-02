/** Query-term utilities shared by the search surfaces. */

/** Return quoted phrases and bare query terms worth highlighting. */
export function queryTerms(query: string): string[] {
  const out: string[] = [];
  const pattern = /"([^"]+)"|'([^']+)'|(\S+)/g;
  let match = pattern.exec(query);
  while (match !== null) {
    const term = (match[1] ?? match[2] ?? match[3] ?? '').trim();
    if (term.length > 1) out.push(term);
    match = pattern.exec(query);
  }
  // Longest first so "graph search" wins over "graph" when both are present.
  return [...new Set(out.map((t) => t.toLowerCase()))].sort((a, b) => b.length - a.length);
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

export interface Segment {
  readonly text: string;
  readonly hit: boolean;
}

/** Split `text` into alternating plain / matched segments for rendering. */
export function segmentMatches(text: string, terms: readonly string[]): Segment[] {
  if (text === '' || terms.length === 0) return [{ text, hit: false }];
  const pattern = new RegExp(`(${terms.map(escapeRegExp).join('|')})`, 'gi');
  const parts = text.split(pattern);
  const segments: Segment[] = [];
  for (const part of parts) {
    if (part === '') continue;
    const hit = terms.some((term) => term === part.toLowerCase());
    const last = segments[segments.length - 1];
    if (last && last.hit === hit) {
      segments[segments.length - 1] = { text: last.text + part, hit };
    } else {
      segments.push({ text: part, hit });
    }
  }
  return segments.length > 0 ? segments : [{ text, hit: false }];
}

/** True when any term occurs in the value. */
export function fieldMatches(value: unknown, terms: readonly string[]): boolean {
  if (terms.length === 0) return false;
  if (value == null) return false;
  const text = String(value).toLowerCase();
  return terms.some((term) => text.includes(term));
}

/** Return the visible fields that contain at least one query term. */
export function matchedFieldNames(
  row: Record<string, unknown>,
  fields: readonly string[],
  terms: readonly string[],
): string[] {
  return fields.filter((field) => fieldMatches(row[field], terms));
}

/** Trim text to the neighbourhood of the first matched term. */
export function matchWindow(text: string, terms: readonly string[], radius = 90): string {
  const flat = text.replace(/\s+/g, ' ').trim();
  if (terms.length === 0) return flat;
  const lower = flat.toLowerCase();
  let at = -1;
  for (const term of terms) {
    const index = lower.indexOf(term);
    if (index >= 0 && (at < 0 || index < at)) at = index;
  }
  if (at < 0 || at <= radius) return flat;
  const start = Math.max(0, at - Math.floor(radius / 2));
  return `…${flat.slice(start)}`;
}
