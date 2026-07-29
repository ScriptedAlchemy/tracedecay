/**
 * Explorer read-model: normalises the three independent memories the daemon
 * exposes — the code graph, session transcripts, and holographic knowledge —
 * into one comparable result shape.
 *
 * Rules this module exists to enforce:
 *  - No invented ranking. `rank` is only the row's position in its named
 *    response collection; the browser never presents the three endpoints as
 *    one canonical merge.
 *  - No invented text. Display values come from returned fields. Rows without
 *    any usable returned identifier are omitted.
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
    searches: 'name, qualified_name, signature, and file_path',
    browseLabel: 'most-connected symbols',
    railClass: 'bg-accent',
    textClass: 'text-accent',
    facetLabel: 'Symbol kind',
  },
  {
    id: 'sessions',
    label: 'Sessions',
    searches: 'message content and summary text in the active LCM store',
    browseLabel: 'latest session summaries',
    railClass: 'bg-state-partial',
    textClass: 'text-state-partial',
    facetLabel: 'Role',
  },
  {
    id: 'knowledge',
    label: 'Knowledge',
    searches: 'content and tags in a bounded fact overview',
    browseLabel: 'a bounded fact overview',
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
  readonly basis: string;
}

export interface Hit {
  readonly key: string;
  readonly lane: LaneId;
  /** 1-based position in the named response collection. */
  readonly rank: number;
  readonly orderLabel: string;
  readonly title: string;
  /** Payload field the title was read from. */
  readonly titleField: string;
  readonly context?: string;
  readonly contextFields: readonly string[];
  readonly body?: string;
  readonly bodyField?: string;
  /** Value of this lane's pivot dimension for this row. */
  readonly facet?: string;
  readonly stamp?: number;
  readonly stampField?: string;
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
  return rows.flatMap((row, index) => {
    const id = str(row, 'id');
    const name = str(row, 'name') ?? str(row, 'qualified_name') ?? id;
    if (!name) return [];
    const titleField = str(row, 'name')
      ? 'name'
      : str(row, 'qualified_name')
        ? 'qualified_name'
        : 'id';
    const file = str(row, 'file_path');
    const line = num(row, 'start_line');
    const degree = num(row, 'degree');
    const body = str(row, 'signature') ?? str(row, 'doc');
    const bodyField = str(row, 'signature') ? 'signature' : str(row, 'doc') ? 'doc' : undefined;
    const kind = str(row, 'kind');
    const hit: Hit = {
      key: `code:${id ?? index}`,
      lane: 'code',
      rank: index + 1,
      orderLabel: 'graph endpoint rows',
      title: name,
      titleField,
      ...(file ? { context: line == null ? file : `${file}:${line}` } : {}),
      contextFields: file ? (line == null ? ['file_path'] : ['file_path', 'start_line']) : [],
      ...(body && bodyField ? { body, bodyField } : {}),
      ...(kind ? { facet: kind } : {}),
      ...(degree != null
        ? {
            signal: {
              field: 'degree',
              value: degree,
              max,
              display: `${degree.toLocaleString()} edges`,
              basis: 'maximum among loaded graph rows',
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
  let messageRank = 0;
  let summaryRank = 0;
  return rows.flatMap((row, index) => {
    const messageId = str(row, 'message_id');
    const nodeId = str(row, 'node_id');
    const storeId = str(row, 'store_id');
    const isSummary = nodeId != null && messageId == null;
    const rank = isSummary ? ++summaryRank : ++messageRank;
    const snippet = str(row, 'snippet');
    const content = str(row, 'content');
    const summary = str(row, 'summary');
    const session = str(row, 'session_id');
    const text = snippet ?? content ?? summary ?? session ?? messageId ?? nodeId ?? storeId;
    if (!text) return [];
    const titleField = snippet
      ? 'snippet'
      : content
        ? 'content'
        : summary
          ? 'summary'
          : session
            ? 'session_id'
            : messageId
              ? 'message_id'
              : nodeId
                ? 'node_id'
                : 'store_id';
    const role = str(row, 'role') ?? (summary ? 'summary' : undefined);
    const source = str(row, 'source');
    const provider = source ?? str(row, 'provider');
    const providerField = source ? 'source' : provider ? 'provider' : undefined;
    const timestamp = num(row, 'timestamp');
    const latestAt = num(row, 'latest_at');
    const stamp = timestamp ?? latestAt;
    const stampField = timestamp != null ? 'timestamp' : latestAt != null ? 'latest_at' : undefined;
    const hit: Hit = {
      key: `sessions:${messageId ?? nodeId ?? storeId ?? index}`,
      lane: 'sessions',
      rank,
      orderLabel: isSummary ? 'summary-node matches' : 'message matches',
      title: matchWindow(text, terms),
      titleField,
      ...(session
        ? { context: provider ? `${provider} · ${session}` : session }
        : {}),
      contextFields: [
        ...(providerField ? [providerField] : []),
        ...(session ? ['session_id'] : []),
      ],
      ...(role ? { facet: role } : {}),
      ...(stamp != null && stampField ? { stamp, stampField } : {}),
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
  return rows.flatMap((row, index) => {
    const content = str(row, 'content');
    const factId = str(row, 'fact_id');
    const title = content ?? factId;
    if (!title) return [];
    const trust = num(row, 'trust_score');
    const rawTags = row['tags'];
    const tags = Array.isArray(rawTags)
      ? rawTags.map((tag) => String(tag)).filter((tag) => tag !== '')
      : [];
    const category = str(row, 'category');
    const recalled = num(row, 'last_recalled_at');
    const hit: Hit = {
      key: `knowledge:${factId ?? index}`,
      lane: 'knowledge',
      rank: index + 1,
      orderLabel: 'bounded fact endpoint rows',
      title: content ? matchWindow(content, terms) : title,
      titleField: content ? 'content' : 'fact_id',
      ...(tags.length > 0 ? { context: tags.join(' · ') } : {}),
      contextFields: tags.length > 0 ? ['tags'] : [],
      ...(category ? { facet: category } : {}),
      ...(recalled != null ? { stamp: recalled, stampField: 'last_recalled_at' } : {}),
      ...(trust != null
        ? {
            signal: {
              field: 'trust_score',
              value: trust,
              max: 1,
              display: `trust ${trust.toFixed(2)}`,
              basis: 'fixed trust scale from 0 to 1',
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
