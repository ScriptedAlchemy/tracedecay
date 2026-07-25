/**
 * Explorer read-model: normalises the three independent memories the daemon
 * exposes — the code graph, session transcripts, and holographic knowledge —
 * into one comparable result shape.
 *
 * Rules this module exists to enforce:
 *  - No invented ranking. `rank` is nothing but the row's position in the
 *    daemon's own response; `signal` is only ever a field the payload really
 *    carried (graph degree, fact trust), and it always names that field.
 *  - No invented text. Every string is read from the row, never synthesised.
 *  - Missing is missing: absent fields become `undefined`, not placeholders.
 */
import { matchedFieldNames, matchWindow } from '../../ui/search/terms.ts';

export type LaneId = 'code' | 'sessions' | 'knowledge';

export interface LaneSpec {
  readonly id: LaneId;
  readonly label: string;
  /** What this lane searches, in one honest clause. */
  readonly searches: string;
  /** What the lane shows before a query is submitted. */
  readonly browseLabel: string;
  /** Utility class for the lane's identity rail. */
  readonly railClass: string;
  readonly textClass: string;
  /** Name of the pivot dimension this lane's rows carry. */
  readonly facetLabel: string;
}

export const LANES: readonly LaneSpec[] = [
  {
    id: 'code',
    label: 'Code graph',
    searches: 'symbol names, qualified paths, and files',
    browseLabel: 'most-connected symbols',
    railClass: 'bg-accent',
    textClass: 'text-accent',
    facetLabel: 'Symbol kind',
  },
  {
    id: 'sessions',
    label: 'Sessions',
    searches: 'transcript messages across every provider',
    browseLabel: 'latest session summaries',
    railClass: 'bg-state-partial',
    textClass: 'text-state-partial',
    facetLabel: 'Role',
  },
  {
    id: 'knowledge',
    label: 'Knowledge',
    searches: 'durable facts and their evidence',
    browseLabel: 'facts in the memory store',
    railClass: 'bg-state-ready',
    textClass: 'text-state-ready',
    facetLabel: 'Category',
  },
];

export interface Signal {
  /** The payload field this number came from — shown verbatim to the user. */
  readonly field: string;
  readonly value: number;
  /** Largest value seen across the lane's loaded rows, for the proportion. */
  readonly max: number;
  readonly display: string;
}

export interface Hit {
  readonly key: string;
  readonly lane: LaneId;
  /** 1-based position in the daemon's response for this lane. */
  readonly rank: number;
  readonly title: string;
  /** Payload field the title was read from. */
  readonly titleField: string;
  readonly context?: string;
  readonly body?: string;
  /** Value of this lane's pivot dimension for this row. */
  readonly facet?: string;
  readonly stamp?: number;
  readonly signal?: Signal;
  /** Fields whose text observably contains a query term. */
  readonly matchedIn: readonly string[];
  readonly raw: Record<string, unknown>;
}

function str(row: Record<string, unknown>, key: string): string | undefined {
  const value = row[key];
  if (value == null) return undefined;
  const text = String(value).trim();
  return text === '' ? undefined : text;
}

function num(row: Record<string, unknown>, key: string): number | undefined {
  const value = row[key];
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  return undefined;
}

const CODE_MATCH_FIELDS = ['name', 'qualified_name', 'file_path', 'signature', 'doc'];
const SESSION_MATCH_FIELDS = ['snippet', 'content', 'summary', 'session_id'];
const KNOWLEDGE_MATCH_FIELDS = ['content', 'category'];

/** Code-graph rows (graph_service.rs search / overview `top_connected`). */
export function codeHits(
  rows: readonly Record<string, unknown>[],
  terms: readonly string[],
): Hit[] {
  const max = rows.reduce((m, row) => Math.max(m, num(row, 'degree') ?? 0), 0);
  return rows.map((row, index) => {
    const name = str(row, 'name') ?? str(row, 'qualified_name');
    const file = str(row, 'file_path');
    const line = num(row, 'start_line');
    const degree = num(row, 'degree');
    const body = str(row, 'signature') ?? str(row, 'doc');
    const kind = str(row, 'kind');
    const hit: Hit = {
      key: `code:${str(row, 'id') ?? index}`,
      lane: 'code',
      rank: index + 1,
      title: name ?? `row ${index + 1}`,
      titleField: str(row, 'name') ? 'name' : 'qualified_name',
      ...(file ? { context: line == null ? file : `${file}:${line}` } : {}),
      ...(body ? { body } : {}),
      ...(kind ? { facet: kind } : {}),
      ...(degree != null
        ? {
            signal: {
              field: 'degree',
              value: degree,
              max,
              display: `${degree.toLocaleString()} edges`,
            },
          }
        : {}),
      matchedIn: matchedFieldNames(row, CODE_MATCH_FIELDS, terms),
      raw: row,
    };
    return hit;
  });
}

/** Transcript rows (lcm_api.rs search `matches.messages`) and, in browse mode,
 * the overview's `latest_summary_nodes`. */
export function sessionHits(
  rows: readonly Record<string, unknown>[],
  terms: readonly string[],
): Hit[] {
  return rows.map((row, index) => {
    const text = str(row, 'snippet') ?? str(row, 'content') ?? str(row, 'summary');
    const session = str(row, 'session_id');
    const role = str(row, 'role') ?? (str(row, 'summary') ? 'summary' : undefined);
    const provider = str(row, 'source') ?? str(row, 'provider');
    const stamp = num(row, 'timestamp') ?? num(row, 'latest_at');
    const hit: Hit = {
      key: `sessions:${str(row, 'message_id') ?? str(row, 'node_id') ?? str(row, 'store_id') ?? index}`,
      lane: 'sessions',
      rank: index + 1,
      title: text ? matchWindow(text, terms) : (session ?? `row ${index + 1}`),
      titleField: str(row, 'snippet') ? 'snippet' : str(row, 'content') ? 'content' : 'summary',
      ...(session
        ? { context: provider ? `${provider} · ${session}` : session }
        : {}),
      ...(role ? { facet: role } : {}),
      ...(stamp != null ? { stamp } : {}),
      matchedIn: matchedFieldNames(row, SESSION_MATCH_FIELDS, terms),
      raw: row,
    };
    return hit;
  });
}

/** Knowledge rows (facts.rs fact_summary_json). */
export function knowledgeHits(
  rows: readonly Record<string, unknown>[],
  terms: readonly string[],
): Hit[] {
  return rows.map((row, index) => {
    const content = str(row, 'content');
    const trust = num(row, 'trust_score');
    const tags = Array.isArray(row['tags'])
      ? (row['tags'] as unknown[]).map(String).filter((t) => t !== '')
      : [];
    const category = str(row, 'category');
    const recalled = num(row, 'last_recalled_at');
    const hit: Hit = {
      key: `knowledge:${str(row, 'fact_id') ?? index}`,
      lane: 'knowledge',
      rank: index + 1,
      title: content ? matchWindow(content, terms) : `fact ${str(row, 'fact_id') ?? index}`,
      titleField: 'content',
      ...(tags.length > 0 ? { context: tags.join(' · ') } : {}),
      ...(category ? { facet: category } : {}),
      ...(recalled != null ? { stamp: recalled } : {}),
      ...(trust != null
        ? {
            signal: {
              field: 'trust_score',
              value: trust,
              max: 1,
              display: `trust ${trust.toFixed(2)}`,
            },
          }
        : {}),
      matchedIn: matchedFieldNames(row, KNOWLEDGE_MATCH_FIELDS, terms),
      raw: row,
    };
    return hit;
  });
}

export interface FacetCount {
  readonly id: string;
  readonly label: string;
  readonly count: number;
}

/** Pivot counts over the rows actually loaded — never over the whole index. */
export function facetCounts(hits: readonly Hit[]): FacetCount[] {
  const counts = new Map<string, number>();
  for (const hit of hits) {
    if (!hit.facet) continue;
    counts.set(hit.facet, (counts.get(hit.facet) ?? 0) + 1);
  }
  return [...counts.entries()]
    .map(([id, count]) => ({ id, label: id, count }))
    .sort((a, b) => b.count - a.count || a.label.localeCompare(b.label));
}

/** Compact relative age for a unix-seconds stamp. */
export function relativeTime(epochSeconds: number | undefined): string | undefined {
  if (epochSeconds == null || !Number.isFinite(epochSeconds)) return undefined;
  const delta = Date.now() / 1000 - epochSeconds;
  if (delta < 0) return 'now';
  if (delta < 90) return 'now';
  if (delta < 3600) return `${Math.round(delta / 60)}m`;
  if (delta < 86_400) return `${Math.round(delta / 3600)}h`;
  if (delta < 30 * 86_400) return `${Math.round(delta / 86_400)}d`;
  return `${Math.round(delta / (30 * 86_400))}mo`;
}
