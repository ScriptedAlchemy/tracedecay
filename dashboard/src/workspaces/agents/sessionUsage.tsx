import type {
  AnalyticsSubagentNodeV1,
  AnalyticsSubagentTreePayloadV1,
  ProviderUsageCoverageV1,
} from '../../contracts/generated.ts';
import { cn } from '../../ui/cn';
import type { TopologyMark } from './delegationTopology.ts';

/**
 * Provider-reported usage per session, as the subagent tree carries it.
 *
 * Three states a reader must be able to tell apart: `measured` (the provider
 * reported counters for this session), `partial` (it reported some, but the
 * aggregate says it is incomplete), and `absent` (no usage row joins this
 * session). Whether `absent` means "none recorded" or "the read failed" is the
 * tree's `usage_coverage`, printed once in the legend, never guessed per node.
 */
export type SessionUsage =
  | { readonly state: 'absent' }
  | {
      readonly state: 'measured' | 'partial';
      readonly total: number | null;
      readonly events: number;
      readonly split: ReadonlyArray<readonly [string, number | null]>;
    };

/** The tree-level coverage, or `unreported` when the reading predates it. */
export type UsageCoverage = ProviderUsageCoverageV1 | 'unreported';

export function sessionUsage(node: AnalyticsSubagentNodeV1): SessionUsage {
  const usage = node.usage;
  if (usage == null) return { state: 'absent' };
  const { counters } = usage;
  return {
    state: usage.complete ? 'measured' : 'partial',
    total: counters.total_tokens,
    events: usage.usage_events,
    split: [
      ['input', counters.input_tokens],
      ['output', counters.output_tokens],
      ['cache read', counters.cache_read_tokens],
      ['cache write', counters.cache_write_tokens],
      ['reasoning', counters.reasoning_tokens],
    ],
  };
}

export function usageCoverage(payload: AnalyticsSubagentTreePayloadV1): UsageCoverage {
  return payload.usage_coverage ?? 'unreported';
}

export function tokenCount(value: number | null): string {
  return value === null ? 'unreported' : `${value.toLocaleString()} tokens`;
}

/** The mark-level label: the total with its unit, or the typed gap. */
export function usageTotalLabel(usage: SessionUsage): string {
  return usage.state === 'absent' ? 'tokens absent' : tokenCount(usage.total);
}

/** Sum of the measured members' totals across a bundle, with how many joined. */
export function bundleUsage(members: readonly AnalyticsSubagentNodeV1[]): {
  readonly total: number;
  readonly measured: number;
  readonly partial: boolean;
} {
  let total = 0;
  let measured = 0;
  let partial = false;
  for (const member of members) {
    const usage = sessionUsage(member);
    if (usage.state === 'absent') continue;
    measured += 1;
    total += usage.total ?? 0;
    partial ||= usage.state === 'partial' || usage.total === null;
  }
  return { total, measured, partial };
}

export function coverageLabel(coverage: UsageCoverage): string {
  switch (coverage) {
    case 'complete':
      return 'usage read complete · absent means none recorded';
    case 'partial':
      return 'usage read partial · absent may be unread';
    case 'unavailable':
      return 'usage read unavailable · every session absent';
    case 'unreported':
      return 'usage coverage not reported by this reading';
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}

/** The typed-state partial marker: amber, hatched, and the word. */
export function PartialMark({ className }: { className?: string }) {
  return (
    <span
      className={cn(
        'inline-flex items-center border border-state-partial/70 px-1 font-mono text-3xs uppercase leading-none tracking-[0.08em] text-state-partial',
        className,
      )}
      style={{
        backgroundImage:
          'repeating-linear-gradient(135deg, color-mix(in srgb, var(--color-state-partial) 24%, transparent) 0 2px, transparent 2px 5px)',
      }}
      data-usage-partial="true"
    >
      partial
    </span>
  );
}

/** Printed once per view beside the mark legend. */
export function UsageCoverageLegend({ coverage }: { coverage: UsageCoverage }) {
  return (
    <span className="inline-flex items-center gap-1.5" data-usage-coverage={coverage}>
      {coverage === 'partial' ? <PartialMark /> : null}
      <span className={cn(coverage === 'unavailable' && 'text-state-locked')}>tokens · {coverageLabel(coverage)}</span>
    </span>
  );
}

/** A mark's usage in place: the total with its unit (a bundle sums its
 * measured members and says how many joined), then the partial marker. */
export function UsageLabel({ mark }: { mark: TopologyMark }) {
  let text: string;
  let state: SessionUsage['state'];
  if (mark.kind === 'bundle') {
    const summed = bundleUsage(mark.members);
    state = summed.measured === 0 ? 'absent' : summed.partial ? 'partial' : 'measured';
    text =
      summed.measured === 0
        ? 'tokens absent'
        : `${summed.total.toLocaleString()} tokens · ${summed.measured} of ${mark.members.length}`;
  } else {
    const usage = sessionUsage(mark.node);
    state = usage.state;
    text = usageTotalLabel(usage);
  }
  return (
    <span className="inline-flex shrink-0 items-center gap-1 whitespace-nowrap" data-usage-state={state}>
      <span className={cn('font-mono tabular-nums', state === 'absent' && 'opacity-70')} data-usage-total>
        {text}
      </span>
      {state === 'partial' ? <PartialMark /> : null}
    </span>
  );
}
