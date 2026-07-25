/**
 * The weave: sessions composed into vertical threads over a real time axis.
 *
 * Pure — no DOM, no clock of its own. The renderer only draws what this
 * returns, and the tests hit this module directly, which is the same split
 * `brain/field.ts` uses and the reason both surfaces can be checked for
 * honesty without a browser.
 *
 * What each channel encodes (and the caption on the surface says the same
 * words, so picture and prose cannot drift):
 *
 *   y      — TIME, flowing downward. `started_at` from the sessions store,
 *            placed on a printed axis. This is the only continuous
 *            measurement on the field and it is exact.
 *
 *   x      — HOST. One column per provider, ordered by how many threads each
 *            actually carries. Within a column a thread takes the first
 *            sub-column whose previous thread has already finished, so two
 *            threads never overlap on screen. That sub-column offset is
 *            PACKING, not a measurement: it costs nothing because it never
 *            leaves the host's own band, exactly as a body in the Brain field
 *            may shift inside its recency column but never across one.
 *
 *   width  — MESSAGE COUNT, log-scaled. The store's own number; a thread with
 *            more turns in it is a thicker thread.
 *
 *   solidity — EVIDENCE QUALITY, per plan 11a's fill-pattern axis. A thread
 *            whose END the store serves (`last_message_at`) is drawn solid
 *            from start to end: its extent is measured. A thread whose end is
 *            NOT served is drawn as a solid head with an open, dashed tail:
 *            it started here, and how long it ran is unknown. On the real
 *            profile most threads are open, and that is the single most
 *            important true thing this surface has to say about its own data.
 *            Drawing them all as points, or all as some default duration,
 *            would erase it.
 *
 * There is no weft. Every crossing the plan asks the Loom to weave — a
 * session landing on a commit, on an edited file, on a branch or a PR — is a
 * relation the daemon does not serve to the dashboard (see `WEFT_SOURCES`).
 * The surface prints that list rather than drawing a single invented crossing.
 */
import { packTrack, type LoomSpan } from './tracks.ts';
import type { LoomSession } from './contracts.ts';

/** One session, reduced to the quantities the weave actually draws. */
export interface WeaveThread {
  id: string;
  label: string;
  /** Provider — the host that ran the session. Picks the column and the hue. */
  host: string;
  /** Epoch seconds. Measured. */
  start: number;
  /** Epoch seconds when the store served an end later than the start; null
   * when it served nothing usable. Never inferred. */
  end: number | null;
  /** Measured message count; zero is a real reading, not a missing one. */
  messages: number;
  isSubagent: boolean;
  /** Distinct models named on the session's own accounting rows. */
  models: string[];
}

export interface PlacedThread extends WeaveThread {
  /** Host column index. */
  column: number;
  /** Sub-column inside the host band. Packing, not a measurement. */
  lane: number;
  /** Message count as a 0..1 fraction of the heaviest thread, log-scaled. */
  weight: number;
  /** The store served no end: draw an open, dashed tail. */
  openEnded: boolean;
  /** The store served zero messages: draw hollow, never at zero width. */
  hollow: boolean;
}

export interface HostColumn {
  id: string;
  label: string;
  /** Threads in this column. */
  count: number;
  /** Summed measured messages across them. */
  messages: number;
  /** Sub-columns the column needs to keep its threads from overlapping. */
  lanes: number;
}

export interface WeaveExtent {
  start: number;
  end: number;
}

export interface Weave {
  threads: PlacedThread[];
  hosts: HostColumn[];
  /** Full time extent, or null when nothing carried a usable start. */
  extent: WeaveExtent | null;
  /** Heaviest measured message count, so a caller can rank against the same
   * ceiling the widths were computed from. */
  messageCeiling: number;
  /** Threads whose end the store did not serve. */
  openEndedCount: number;
  /** Threads the store reports at zero messages. */
  hollowCount: number;
  /** Total sub-columns across every host — the weave's width. */
  laneTotal: number;
}

/**
 * A causal landing the plan asks the Loom to weave, and where it actually
 * lives. Every entry here is a relation TraceDecay genuinely records and does
 * NOT serve to the dashboard, so the surface can name the gap precisely
 * instead of shrugging. Printed as `unsupported` chips, one per row.
 *
 * This list is the honest weft. It is not a TODO: it is the reading.
 */
export const WEFT_SOURCES: ReadonlyArray<{
  id: string;
  label: string;
  /** Where the relation is recorded in the daemon. */
  store: string;
  /** Why it cannot be drawn. */
  detail: string;
}> = [
  {
    id: 'session-commit',
    label: 'Session ↔ commit',
    store: 'src/sessions/git_correlation.rs',
    detail: 'recorded with span_overlap_kind; no dashboard route',
  },
  {
    id: 'session-file',
    label: 'Session → edited file',
    store: 'sessions.metadata_json $.edited_files[]',
    detail: 'Claude rollup only; no dashboard route',
  },
  {
    id: 'branch-event',
    label: 'Branch & worktree events',
    store: 'project registry',
    detail: 'names served, no event times',
  },
  {
    id: 'pull-request',
    label: 'Pull requests & review',
    store: '—',
    detail: 'not recorded by the daemon',
  },
  {
    id: 'ci-outcome',
    label: 'CI & release outcomes',
    store: '—',
    detail: 'not recorded by the daemon',
  },
];

/** Distinct model names on a session's accounting rows, in wire order and
 * without the null placeholder the daemon uses for untagged turns. */
function modelsOf(session: LoomSession): string[] {
  const out: string[] = [];
  for (const row of session.models ?? []) {
    const model = row.model;
    if (typeof model === 'string' && model.length > 0 && !out.includes(model)) {
      out.push(model);
    }
  }
  return out;
}

/**
 * Reduce wire rows to threads. A row without a usable start is DROPPED rather
 * than placed at an invented time — there is no honest y for it — and the
 * caller is told how many were dropped so the count can be printed.
 */
export function threadsFrom(sessions: readonly LoomSession[]): {
  threads: WeaveThread[];
  undated: number;
} {
  const threads: WeaveThread[] = [];
  let undated = 0;
  for (const session of sessions) {
    const start = Number(session.started_at);
    if (!Number.isFinite(start) || start <= 0) {
      undated += 1;
      continue;
    }
    const rawEnd = session.last_message_at;
    // An end equal to the start is not a duration, it is the same instant
    // recorded twice; treating it as a one-second span would draw a mark that
    // claims a measurement nobody made.
    const end =
      typeof rawEnd === 'number' && Number.isFinite(rawEnd) && rawEnd > start
        ? rawEnd
        : null;
    const messages = Number(session.messages);
    threads.push({
      id: session.session_id,
      label: session.title?.trim() || session.session_id,
      host: session.provider || 'unknown',
      start,
      end,
      messages: Number.isFinite(messages) && messages > 0 ? messages : 0,
      isSubagent: session.is_subagent === true,
      models: modelsOf(session),
    });
  }
  return { threads, undated };
}

/** Full time extent across every thread, or null when there is nothing to
 * span. Open threads contribute only their start, because that is all that
 * was measured. */
export function extentOf(threads: readonly WeaveThread[]): WeaveExtent | null {
  let start = Infinity;
  let end = -Infinity;
  for (const thread of threads) {
    if (thread.start < start) start = thread.start;
    if (thread.start > end) end = thread.start;
    if (thread.end != null && thread.end > end) end = thread.end;
  }
  if (!Number.isFinite(start) || !Number.isFinite(end)) return null;
  // A single instant has no extent to scale against; give the axis an hour so
  // the one thread on it lands somewhere readable rather than dividing by zero.
  if (end - start < 3600) end = start + 3600;
  return { start, end };
}

/**
 * Compose threads into the weave. Deterministic: the same rows always produce
 * the same columns, lanes and widths, so a screenshot is stable and a refetch
 * never reshuffles the picture under the reader.
 *
 * `minGapSeconds` is the time-space equivalent of a few pixels — two threads
 * closer together than this would touch on screen, so packing treats them as
 * overlapping and gives them separate sub-columns. It is a parameter rather
 * than a constant because only the renderer knows the current scale.
 */
export function composeWeave(
  sessions: readonly LoomSession[],
  minGapSeconds = 0,
): Weave & { undated: number } {
  const { threads, undated } = threadsFrom(sessions);
  const extent = extentOf(threads);
  const messageCeiling = threads.reduce(
    (max, thread) => Math.max(max, thread.messages),
    0,
  );

  // Group by host, then order columns by how much of the weave each host
  // actually carries. Ties break on name so the order never depends on the
  // order the daemon happened to return rows in.
  const byHost = new Map<string, WeaveThread[]>();
  for (const thread of threads) {
    const bucket = byHost.get(thread.host);
    if (bucket) bucket.push(thread);
    else byHost.set(thread.host, [thread]);
  }
  const hostIds = [...byHost.keys()].sort((a, b) => {
    const delta = (byHost.get(b)?.length ?? 0) - (byHost.get(a)?.length ?? 0);
    return delta !== 0 ? delta : a.localeCompare(b);
  });

  const ceilingLog = Math.log1p(Math.max(messageCeiling, 1));
  const hosts: HostColumn[] = [];
  const placed: PlacedThread[] = [];
  let laneTotal = 0;

  hostIds.forEach((hostId, column) => {
    const bucket = byHost.get(hostId) ?? [];
    // Pack on the same interval machinery the track engine uses: each thread
    // drops into the first sub-column whose previous thread has ended.
    const spans: LoomSpan[] = bucket.map((thread) => ({
      id: thread.id,
      start: thread.start,
      end: thread.end ?? thread.start,
      label: thread.label,
      weight: thread.messages,
    }));
    const lanes = packTrack(spans, minGapSeconds);
    const laneById = new Map<string, number>();
    lanes.forEach((lane, index) => {
      for (const span of lane) laneById.set(span.id, index);
    });

    let messages = 0;
    for (const thread of bucket) {
      messages += thread.messages;
      placed.push({
        ...thread,
        column,
        lane: laneById.get(thread.id) ?? 0,
        weight:
          thread.messages > 0 && ceilingLog > 0
            ? Math.log1p(thread.messages) / ceilingLog
            : 0,
        openEnded: thread.end == null,
        hollow: thread.messages === 0,
      });
    }

    const laneCount = Math.max(lanes.length, 1);
    laneTotal += laneCount;
    hosts.push({
      id: hostId,
      label: hostId,
      count: bucket.length,
      messages,
      lanes: laneCount,
    });
  });

  // Earliest first, so the draw order runs down the axis and a reader stepping
  // through the accessible table moves forward in time.
  placed.sort((a, b) => a.start - b.start || a.id.localeCompare(b.id));

  return {
    threads: placed,
    hosts,
    extent,
    messageCeiling,
    openEndedCount: placed.filter((thread) => thread.openEnded).length,
    hollowCount: placed.filter((thread) => thread.hollow).length,
    laneTotal: Math.max(laneTotal, 1),
    undated,
  };
}

/* -------------------------------------------------------------------------
 * The chain of a selected thread
 * ---------------------------------------------------------------------- */

export interface ChainStep {
  id: string;
  /** `user` | `assistant` | `system` | whatever the store says. */
  role: string;
  /** Tool named on this turn, when the store named one. */
  tool: string | null;
  tokens: number | null;
  /** First line of the turn, trimmed for the rail. */
  excerpt: string;
}

export interface ChainSummary {
  steps: ChainStep[];
  /** Turns per role, ordered by count. */
  roles: Array<{ role: string; count: number }>;
  /** Tool invocations per tool, ordered by count — the measured "tools" leg
   * of prompt → tools → edits → commits. */
  tools: Array<{ tool: string; count: number }>;
  /** Total turns the store reports for the session, which may exceed the page
   * of steps returned. */
  messageCount: number;
  tokenEstimate: number;
  /** True when the store served at least one message timestamp. On the real
   * profile this is false everywhere, which is why the chain is ordinal-
   * ordered and says so. */
  timestamped: boolean;
  /** The page did not reach the end of the session. */
  truncated: boolean;
}

/** Reduce a session-detail payload to the chain the rail draws. Ordering is
 * the store's `ordinal` where present, falling back to wire order — never to
 * a timestamp, because there is none. */
export function summarizeChain(
  messages: readonly {
    message_id: string;
    role?: string | null | undefined;
    content?: string | null | undefined;
    ordinal?: number | null | undefined;
    timestamp?: number | null | undefined;
    tool_name?: string | null | undefined;
    token_estimate?: number | null | undefined;
  }[],
  counts?: { message_count?: number; token_estimate_total?: number } | undefined,
  truncated = false,
): ChainSummary {
  const ordered = messages
    .map((message, index) => ({ message, index }))
    .sort((a, b) => {
      const left = a.message.ordinal;
      const right = b.message.ordinal;
      if (typeof left === 'number' && typeof right === 'number' && left !== right) {
        return left - right;
      }
      return a.index - b.index;
    })
    .map(({ message }) => message);

  const roleCounts = new Map<string, number>();
  const toolCounts = new Map<string, number>();
  let timestamped = false;

  const steps: ChainStep[] = ordered.map((message) => {
    const role = (message.role ?? 'unknown').trim() || 'unknown';
    roleCounts.set(role, (roleCounts.get(role) ?? 0) + 1);
    const tool =
      typeof message.tool_name === 'string' && message.tool_name.length > 0
        ? message.tool_name
        : null;
    if (tool) toolCounts.set(tool, (toolCounts.get(tool) ?? 0) + 1);
    if (typeof message.timestamp === 'number' && message.timestamp > 0) {
      timestamped = true;
    }
    const tokens =
      typeof message.token_estimate === 'number' && message.token_estimate >= 0
        ? message.token_estimate
        : null;
    return {
      id: message.message_id,
      role,
      tool,
      tokens,
      excerpt: excerptOf(message.content),
    };
  });

  const rank = <T extends { count: number }>(entries: T[]): T[] =>
    entries.sort((a, b) => b.count - a.count);

  return {
    steps,
    roles: rank([...roleCounts].map(([role, count]) => ({ role, count }))),
    tools: rank([...toolCounts].map(([tool, count]) => ({ tool, count }))),
    messageCount: counts?.message_count ?? steps.length,
    tokenEstimate: counts?.token_estimate_total ?? 0,
    timestamped,
    truncated,
  };
}

/** One line of a turn, short enough for a rail and long enough to identify.
 * Whitespace is collapsed so a pasted command does not print as a paragraph. */
function excerptOf(content: string | null | undefined): string {
  if (typeof content !== 'string') return '';
  const line = content.replace(/\s+/g, ' ').trim();
  return line.length > 140 ? `${line.slice(0, 139)}…` : line;
}
