/**
 * The chain of a selected session: prompt → turns → tools, reduced from the
 * loaded LCM transcript page.
 *
 * Pure, no DOM, no clock. Ordering is the store's `ordinal` where present,
 * falling back to wire order, never to a timestamp, because there is
 * usually none. The same ordering helper feeds the field's transcript events,
 * the playback cursor and this summary, so they cannot drift apart.
 */
import type { LcmMessageV1 } from '../../contracts/generated.ts';
import { orderMessages } from '../../viz/temporal/journey.ts';

export interface ChainStep {
  id: string;
  /** `user` | `assistant` | `system` | whatever the store says. */
  role: string;
  /** Tool named on this turn, when the store named one. */
  tool: string | null;
  tokenCount: number | null;
  tokenCountProvenance: 'o200k_approximate' | null;
  /** First line of the turn, trimmed for the rail. */
  excerpt: string;
}

export interface ChainSummary {
  steps: ChainStep[];
  /** Turns per role, ordered by count. */
  roles: Array<{ role: string; count: number }>;
  /** Tool invocations per tool, ordered by count, the measured "tools" leg
   * of prompt → tools → edits → commits. */
  tools: Array<{ tool: string; count: number }>;
  /** Total turns the store reports for the session, which may exceed the page
   * of steps returned. */
  messageCount: number;
  /** True when the store served at least one message timestamp. On the real
   * profile this is false everywhere, which is why the chain is ordinal-
   * ordered and says so. */
  timestamped: boolean;
  /** The page did not reach the end of the session. */
  truncated: boolean;
}

/**
 * Every field is drawn from the generated `LcmMessageV1` wire contract, so a
 * contract change reaches this module through the type system instead of
 * drifting past a hand-written mirror. All but the id stay optional because
 * the chain reads what a row actually has, absent quantities stay absent.
 */
export type ChainMessageInput = Pick<LcmMessageV1, 'message_id'> &
  Partial<
    Pick<
      LcmMessageV1,
      | 'role'
      | 'content'
      | 'ordinal'
      | 'timestamp'
      | 'tool_name'
      | 'token_count'
      | 'token_count_provenance'
    >
  >;

/** Reduce a session-detail page to the chain the rail draws. */
export function summarizeChain(
  messages: readonly ChainMessageInput[],
  counts?: { message_count?: number } | undefined,
  truncated = false,
): ChainSummary {
  const ordered = orderMessages(messages);

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
    const validProvenance =
      message.token_count_provenance === 'o200k_approximate'
        ? message.token_count_provenance
        : null;
    const tokenCount =
      validProvenance != null &&
      typeof message.token_count === 'number' &&
      message.token_count >= 0
        ? message.token_count
        : null;
    return {
      id: message.message_id,
      role,
      tool,
      tokenCount,
      tokenCountProvenance: tokenCount == null ? null : validProvenance,
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
