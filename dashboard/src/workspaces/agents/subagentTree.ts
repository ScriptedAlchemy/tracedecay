import type {
  AnalyticsSubagentNodeV1,
  AnalyticsSubagentTreePayloadV1,
} from '../../contracts/generated.ts';

/**
 * The subagent tree: parent/child session edges, read from the session store.
 *
 * This is the measure the sibling `/agents` rollup cannot give. That route
 * counts sessions per managed agent, which says how often each agent was
 * delegated to but nothing at all about who delegated to whom — every count on
 * it is an island. Delegation is an edge, and an edge cannot be recovered from
 * two counts after the fact, which is why `/api/plugins/analytics/subagent-tree`
 * is served separately rather than folded into the rollup.
 *
 * The daemon does the tree assembly and hands back a PRE-ORDER flattening:
 * every node appears after its own parent and before that parent's later
 * siblings. So `depth` alone is enough to draw the tree, and nothing here
 * re-derives edges client-side — a second assembly could disagree with the
 * first, and then the drawn shape would be this build's opinion rather than the
 * store's reading.
 *
 * What this module does add is the grouping the drawing needs (where one tree
 * ends and the next begins) and the sentences that keep the four link kinds
 * apart. Those four are not decoration: a session with no parent and a session
 * whose parent was never ingested both sit at the left margin, and only the
 * first is a root. Captioning the second as a root would assert a delegation
 * boundary nobody observed.
 */

/** One delegation tree: a top and everything beneath it, in pre-order. */
export interface SubagentTreeGroup {
  readonly top: AnalyticsSubagentNodeV1;
  /** Includes `top` as its first entry. */
  readonly nodes: readonly AnalyticsSubagentNodeV1[];
}

/**
 * Split the pre-order node list into its trees.
 *
 * A new tree starts at every `depth === 0` entry. This is a partition of the
 * input, not a filter: every node lands in exactly one group, so the groups sum
 * back to the reading and no session can go missing between the payload and the
 * screen.
 */
export function groupSubagentTrees(
  nodes: readonly AnalyticsSubagentNodeV1[],
): readonly SubagentTreeGroup[] {
  const groups: SubagentTreeGroup[] = [];
  let current: AnalyticsSubagentNodeV1[] | null = null;
  for (const node of nodes) {
    if (node.depth === 0 || current === null) {
      current = [node];
      groups.push({ top: node, nodes: current });
      continue;
    }
    current.push(node);
  }
  return groups;
}

/** What a node should be called. Never a fabricated name. */
export function subagentLabel(node: AnalyticsSubagentNodeV1): string {
  return node.agent ?? node.title ?? node.session_id;
}

/**
 * Elapsed seconds for a session, when both ends were recorded.
 *
 * The store's session stamps are Unix SECONDS (capture parses provider
 * timestamps to seconds and normalizes millisecond inputs down before
 * storing), so this subtraction is already in seconds and must not be divided
 * by a thousand on its way to a reader.
 *
 * `null` means at least one end is unrecorded — an open session and a session
 * that ended instantly are different facts, and neither may be drawn as zero.
 */
export function subagentElapsedSeconds(node: AnalyticsSubagentNodeV1): number | null {
  if (node.started_at == null || node.ended_at == null) return null;
  const elapsed = node.ended_at - node.started_at;
  // A negative span is a contradiction in the store, not a duration.
  return elapsed >= 0 ? elapsed : null;
}

/** A per-link-kind census, so the caption can state what it is standing on. */
export interface SubagentTreeCensus {
  readonly nodes: number;
  readonly sessionsRead: number;
  readonly roots: number;
  readonly edges: number;
  readonly missingParents: number;
  readonly cycles: number;
  readonly maxDepth: number;
  readonly truncated: boolean;
  /** True when the reading holds sessions but not one delegation edge. */
  readonly flat: boolean;
}

export function subagentTreeCensus(
  payload: AnalyticsSubagentTreePayloadV1,
): SubagentTreeCensus {
  return {
    nodes: payload.nodes.length,
    sessionsRead: payload.sessions_read,
    roots: payload.root_count,
    edges: payload.edge_count,
    missingParents: payload.missing_parent_count,
    cycles: payload.cycle_count,
    maxDepth: payload.max_depth,
    truncated: payload.truncated,
    flat: payload.nodes.length > 0 && payload.edge_count === 0,
  };
}
