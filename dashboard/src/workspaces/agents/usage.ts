/**
 * The measured field behind the Agents workspace, as pure functions.
 *
 * Everything here exists because the analytics endpoints serve one shape of
 * data — a single runaway leader over a long tail — that a linear bar chart is
 * physically unable to render. `tracedecay_mcp` carries 6,774 events in the
 * same window in which `workflow_skill` carries 1. Drawn linearly, eleven of
 * the twelve rows are a sliver one pixel tall, and the reader learns nothing
 * they could not have read off the first number. Plan 11a's degenerate-
 * distribution rule applies: the dominance is SAID, and what remains after the
 * leader is drawn on a scale that can hold it.
 */

export interface UsageRow {
  kind: string;
  category: string;
  events: number;
}

export interface Dominance {
  /** Sum of every row's events — the denominator the shares are taken against. */
  total: number;
  leader: UsageRow | null;
  /** The leader's share of `total`, 0–1. Null when there is nothing to divide. */
  leaderShare: number | null;
  /** Leader events divided by the smallest non-zero row's events. */
  spread: number | null;
  /**
   * True when one row is large enough that a linear axis shared with the rest
   * would render the rest as nothing. This is the condition under which the
   * view must state the dominance rather than draw it.
   */
  dominant: boolean;
  /** Every row except the leader, in descending order. */
  rest: UsageRow[];
}

/**
 * Rank the rows and measure how lopsided they are.
 *
 * `threshold` is the leader share above which a linear rendering stops being
 * informative. 0.6 is deliberate rather than tuned: past it the second-placed
 * row can never exceed 40% of the axis and everything below it is compressed
 * into the bottom third, which is exactly the failure this guard exists to
 * catch.
 */
export function summarizeDominance(
  rows: readonly UsageRow[],
  threshold = 0.6,
): Dominance {
  const ranked = [...rows].sort((a, b) => b.events - a.events);
  const total = ranked.reduce((sum, row) => sum + Math.max(0, row.events), 0);
  const leader = ranked[0] ?? null;
  const leaderShare = total > 0 && leader ? leader.events / total : null;
  const smallest = ranked.reduce(
    (min: number | null, row) =>
      row.events > 0 ? (min == null ? row.events : Math.min(min, row.events)) : min,
    null,
  );
  const spread =
    leader != null && smallest != null && smallest > 0 ? leader.events / smallest : null;
  return {
    total,
    leader,
    leaderShare,
    spread,
    dominant: leaderShare != null && leaderShare >= threshold && ranked.length > 1,
    rest: ranked.slice(1),
  };
}

/** A share as a whole-number percent, or null when the share is unknown. */
export function percent(share: number | null | undefined): number | null {
  if (share == null || !Number.isFinite(share)) return null;
  return Math.round(share * 100);
}

export interface EventWindow {
  /** How many events the endpoint actually counted. */
  events: number | null;
  /** Whether that count is the endpoint's own cap rather than the true total. */
  capped: boolean;
  /** Events per hour, straight off the diagnostics payload. */
  perHour: number | null;
  /**
   * How long the counted events span, in hours. DERIVED — the endpoint serves
   * a count and a rate but never the window's own bounds, so this is the one
   * over the other and is labelled as derived wherever it is printed.
   */
  spanHours: number | null;
}

/**
 * `ANALYTICS_EVENT_LIMIT` in src/dashboard/analytics_api.rs. The usage and
 * diagnostics endpoints both read `ORDER BY timestamp DESC, id DESC LIMIT
 * 10000`, so an `event_count` of exactly this value means "the most recent
 * 10,000", never "10,000 in total". The figure was previously printed as a
 * bare stat tile, which asserted the second reading.
 */
export const ANALYTICS_EVENT_LIMIT = 10_000;

export function describeWindow(
  events: number | null | undefined,
  perHour: number | null | undefined,
): EventWindow {
  const count = Number.isFinite(events ?? NaN) ? (events as number) : null;
  const rate = perHour != null && Number.isFinite(perHour) && perHour > 0 ? perHour : null;
  return {
    events: count,
    capped: count != null && count >= ANALYTICS_EVENT_LIMIT,
    perHour: rate,
    spanHours: rate != null && count != null && count > 0 ? count / rate : null,
  };
}

/** A duration in hours, in the shortest form that keeps two significant
 * figures. Returns an em dash rather than a guess when the span is unknown. */
export function formatSpan(hours: number | null | undefined): string {
  if (hours == null || !Number.isFinite(hours) || hours <= 0) return '—';
  if (hours < 1) return `${Math.round(hours * 60)} min`;
  if (hours < 48) return `${hours < 10 ? hours.toFixed(1) : Math.round(hours)} h`;
  const days = hours / 24;
  return `${days < 10 ? days.toFixed(1) : Math.round(days)} d`;
}

export interface FamilyRow {
  family: string;
  usage_events?: number | undefined;
  relevant_events?: number | undefined;
  missed_events?: number | undefined;
  underused?: boolean | undefined;
}

export interface FamilyNote {
  /** Which tools count as this family, per `record_tool_family`. */
  covers: string;
  /**
   * Which non-TraceDecay actions the analyzer counts as a moment where this
   * family SHOULD have been reached for. Null when no such detector exists —
   * in which case `relevant_events` is structurally always zero and the family
   * can never be flagged under-used, which is a property of the analyzer, not
   * a statement about the agent.
   */
  detects: string | null;
}

/**
 * What each family actually is, read off `record_tool_family` in
 * src/analytics.rs. A bare list of four snake_case identifiers told a reader
 * nothing they could act on; these are the substitutions the hint engine is
 * measuring, stated in the terms of the tools involved.
 */
export const FAMILY_NOTES: Readonly<Record<string, FamilyNote>> = {
  code_context: {
    covers: 'tracedecay_context · _node · _files',
    detects: 'Read, cat, sed',
  },
  code_search: {
    covers: 'tracedecay_search · _grep · find_exact_symbol',
    detects: 'grep, rg, glob, shell search',
  },
  call_graph: {
    covers: 'tracedecay_call* · _graph',
    detects: null,
  },
  impact_analysis: {
    covers: 'tracedecay_impact · _affected',
    detects: null,
  },
};

export type FamilyState = 'underused' | 'covered' | 'unmeasurable' | 'idle';

export interface FamilyVerdict {
  state: FamilyState;
  /** One line a reader can act on, or decide not to. */
  line: string;
}

/**
 * The verdict for one family.
 *
 * `underused = missed_events > 0` where `missed = relevant − usage`, so a
 * family with no relevance detector is pinned at `false` forever regardless of
 * behaviour. Reporting that as "not under-used" alongside families that are
 * genuinely measured would be a false equivalence; it gets its own state.
 */
export function familyVerdict(row: FamilyRow): FamilyVerdict {
  const note = FAMILY_NOTES[row.family];
  const usage = row.usage_events ?? 0;
  const relevant = row.relevant_events ?? 0;
  const missed = row.missed_events ?? 0;
  if (note && note.detects == null) {
    return {
      state: 'unmeasurable',
      line:
        usage > 0
          ? `${usage.toLocaleString()} calls · no substitute is detected for this family, so it can never be flagged`
          : 'never called · no substitute is detected for this family, so it can never be flagged',
    };
  }
  if (row.underused === true && missed > 0) {
    return {
      state: 'underused',
      line: `${missed.toLocaleString()} moments reached for ${note?.detects ?? 'a plain tool'} instead`,
    };
  }
  if (usage > 0) {
    return {
      state: 'covered',
      line: `${usage.toLocaleString()} calls · ${relevant.toLocaleString()} substitute moments detected`,
    };
  }
  return { state: 'idle', line: 'never called in this window' };
}

/**
 * The single sentence that replaces a rail of four identical "not under-used"
 * rows. Null when at least one family IS flagged, because then the rows carry
 * the reading themselves.
 */
export function familiesSummary(rows: readonly FamilyRow[]): string | null {
  if (rows.length === 0) return null;
  const flagged = rows.filter((row) => row.underused === true && (row.missed_events ?? 0) > 0);
  if (flagged.length > 0) return null;
  const unmeasurable = rows.filter((row) => FAMILY_NOTES[row.family]?.detects == null);
  const measured = rows.length - unmeasurable.length;
  const detected = rows.reduce((sum, row) => sum + (row.relevant_events ?? 0), 0);
  const base = `No family is flagged: ${measured} of ${rows.length} have a substitute detector at all, and ${detected === 0 ? 'it fired zero times' : `it fired ${detected.toLocaleString()} times`} in this window.`;
  return unmeasurable.length > 0
    ? `${base} The other ${unmeasurable.length} cannot be flagged by construction.`
    : base;
}
