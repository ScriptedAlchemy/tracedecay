/**
 * Tool activity, derived from the analytics diagnostics fold.
 *
 * The Agents page already ranks `by_mcp_tool` — WHICH tools were called most.
 * Plan 11 asks for tool activity, which is a different question: how much of
 * what the connected agents did was tool use at all, what kind of tool use it
 * was, and which agent did it. Those live in members of
 * `AnalyticsDiagnosticsPayloadV1` this page was decoding and then dropping:
 * `tool_call_count`, `mcp_tool_call_count`, `tracedecay_call_count`,
 * `by_tool_category`, `ratios`, and `recent_hooks`.
 *
 * Everything here is derivation over served counts. Nothing is estimated, and
 * every figure that cannot be computed from what arrived comes back `null` so
 * the surface can say the read did not carry it rather than print a zero.
 */

/** `{ <label field>: string, count: number }` rows, the shape every `by_*`
 * array on the diagnostics payload uses. Kept loose for the same reason the
 * page's own schema is: a new label field on the wire must not fail the parse. */
export type CountRow = Record<string, unknown>;

function label(row: CountRow, field: string): string {
  const value = row[field];
  return typeof value === 'string' ? value : '';
}

function count(row: CountRow): number {
  const value = Number(row['count'] ?? Number.NaN);
  return Number.isFinite(value) ? value : 0;
}

export interface LabelledCount {
  readonly label: string;
  readonly count: number;
}

/**
 * The members of `AnalyticsDiagnosticsPayloadV1` this surface reads.
 *
 * Structural and every member optional, so the page's own passthrough schema
 * satisfies it without a cast and a field the daemon stops sending becomes an
 * absence the surface states rather than a parse failure that blanks the page.
 */
export interface ToolActivityRead {
  readonly tool_call_count?: number | undefined;
  readonly mcp_tool_call_count?: number | undefined;
  readonly tracedecay_call_count?: number | undefined;
  readonly by_tool_category?: readonly CountRow[] | undefined;
  readonly by_tool?: readonly CountRow[] | undefined;
  readonly ratios?: unknown;
  readonly recent_hooks?: readonly CountRow[] | undefined;
  readonly hook_window?: { readonly truncated?: boolean | undefined } | undefined;
}

/** A `by_*` array as ranked, named counts. Rows with no label are dropped
 * rather than shown as a blank name; they carry no reading. */
export function rankedCounts(rows: readonly CountRow[], field: string): LabelledCount[] {
  return rows
    .map((row) => ({ label: label(row, field), count: count(row) }))
    .filter((row) => row.label !== '')
    .sort((a, b) => b.count - a.count || a.label.localeCompare(b.label));
}

/**
 * How the window's tool calls divide.
 *
 * `mcp` is served directly. `other` is the remainder of `tool_call_count` after
 * the MCP calls, which is a subtraction and therefore only legal when BOTH
 * counts arrived — a remainder computed against a missing total would be the
 * total restated under a second name.
 *
 * `contradiction` is the case the subtraction must not silently absorb: a store
 * reporting more MCP tool calls than tool calls is disagreeing with itself, and
 * clamping the remainder to zero would hide that behind a plausible figure.
 */
export interface ToolCallSplit {
  readonly total: number | null;
  readonly mcp: number | null;
  readonly other: number | null;
  readonly tracedecay: number | null;
  readonly contradiction: boolean;
}

function served(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

export function toolCallSplit(source: {
  tool_call_count?: unknown;
  mcp_tool_call_count?: unknown;
  tracedecay_call_count?: unknown;
}): ToolCallSplit {
  const total = served(source.tool_call_count);
  const mcp = served(source.mcp_tool_call_count);
  const tracedecay = served(source.tracedecay_call_count);
  const contradiction = total != null && mcp != null && mcp > total;
  return {
    total,
    mcp,
    other: total != null && mcp != null && !contradiction ? total - mcp : null,
    tracedecay,
    contradiction,
  };
}

/** Tool calls per message over the window, or `null` when the fold served no
 * ratio. Derived nowhere: the payload carries it, and dividing two capped
 * counts here would produce a rate over a window neither of them describes. */
export function toolCallsPerMessage(ratios: unknown): number | null {
  if (typeof ratios !== 'object' || ratios === null) return null;
  return served((ratios as Record<string, unknown>)['tool_calls_per_message']);
}

/**
 * One agent's tool activity, from the hook tape.
 *
 * `recent_hooks` is the only member of the diagnostics payload that names an
 * AGENT beside a tool, which makes it the only per-agent tool attribution this
 * build can read. It is a recent suffix and never a total — the caller must say
 * so — but a suffix of real attributed rows is a stronger reading than a
 * complete count attributed to nobody.
 */
export interface AgentToolActivityRow {
  readonly agent: string;
  readonly calls: number;
  readonly sessions: number;
  /** Distinct tools this agent was observed reaching for, most-used first. */
  readonly tools: readonly LabelledCount[];
}

interface HookRow {
  agent?: unknown;
  tool_name?: unknown;
  session_id?: unknown;
}

/**
 * Group the hook tape by agent.
 *
 * Rows carrying no agent are excluded rather than bucketed under a placeholder:
 * this surface's whole claim is attribution, and an "unknown" bucket drawn
 * beside named agents reads as an agent called unknown. The count of excluded
 * rows is returned so the caller can state what it left out.
 */
export function agentToolActivity(rows: readonly HookRow[]): {
  readonly agents: readonly AgentToolActivityRow[];
  readonly unattributed: number;
} {
  const tally = new Map<string, { calls: number; sessions: Set<string>; tools: Map<string, number> }>();
  let unattributed = 0;
  for (const row of rows) {
    const agent = typeof row.agent === 'string' ? row.agent.trim() : '';
    if (agent === '') {
      unattributed += 1;
      continue;
    }
    let seat = tally.get(agent);
    if (!seat) {
      seat = { calls: 0, sessions: new Set(), tools: new Map() };
      tally.set(agent, seat);
    }
    seat.calls += 1;
    if (typeof row.session_id === 'string' && row.session_id !== '') {
      seat.sessions.add(row.session_id);
    }
    const tool = typeof row.tool_name === 'string' ? row.tool_name.trim() : '';
    if (tool !== '') seat.tools.set(tool, (seat.tools.get(tool) ?? 0) + 1);
  }
  const agents = [...tally.entries()]
    .map(([agent, seat]) => ({
      agent,
      calls: seat.calls,
      sessions: seat.sessions.size,
      tools: [...seat.tools.entries()]
        .map(([toolLabel, toolCount]) => ({ label: toolLabel, count: toolCount }))
        .sort((a, b) => b.count - a.count || a.label.localeCompare(b.label)),
    }))
    .sort((a, b) => b.calls - a.calls || a.agent.localeCompare(b.agent));
  return { agents, unattributed };
}
