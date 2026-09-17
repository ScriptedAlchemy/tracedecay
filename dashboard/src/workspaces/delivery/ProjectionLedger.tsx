import type { DeliveryOverviewV1 } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type { ReadState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import { laneStateKind, projectionLaneState, type LaneState } from './journey.ts';

/**
 * The eight independently typed Delivery projections, each with its own
 * state and the daemon's reason. One healthy local projection never paints a
 * provider projection green; a `not_published` row names the authority it
 * requires instead of reading as zero.
 */
export const PROJECTION_ORDER = [
  ['changes', 'Changes', 'local Git'],
  ['commits', 'Commits', 'local Git'],
  ['pull_requests', 'Pull requests', 'provider read'],
  ['review_comments', 'Reviews', 'provider read'],
  ['ci_checks', 'CI checks', 'provider read'],
  ['failure_localization', 'Failure localization', 'retained CI localization'],
  ['releases', 'Releases', 'provider read'],
  ['generation_freshness', 'Index freshness', 'code index'],
] as const satisfies readonly (readonly [keyof DeliveryOverviewV1, string, string])[];

export interface ProjectionRow {
  readonly key: keyof DeliveryOverviewV1;
  readonly label: string;
  readonly source: string;
  readonly state: LaneState;
}

export function projectionRows(overview: DeliveryOverviewV1): readonly ProjectionRow[] {
  return PROJECTION_ORDER.map(([key, label, source]) => ({
    key,
    label,
    source,
    state: projectionLaneState(overview[key], label),
  }));
}

export function laneStateDetail(state: LaneState): string | undefined {
  switch (state.kind) {
    case 'served':
      return state.detail;
    case 'served_empty':
    case 'stale':
    case 'partial':
    case 'rate_limited':
    case 'failed':
    case 'denied':
      return state.detail;
    case 'not_published':
      return `requires ${state.requiredAuthority}`;
    case 'unavailable':
      return state.requiredAuthority === undefined
        ? state.detail
        : `${state.detail} · requires ${state.requiredAuthority}`;
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

export function ProjectionLedger({
  overview,
  className,
}: {
  overview: DeliveryOverviewV1;
  className?: string;
}) {
  const rows = projectionRows(overview);
  return (
    <Panel legend="Delivery pipeline · 8 projections" className={className} bodyClassName="p-0">
      <ol className="divide-y divide-edge-subtle">
        {rows.map((row, index) => (
          <li key={row.key} className="flex items-center gap-3 px-3 py-2">
            <span className="td-value w-5 shrink-0 text-3xs text-text-muted" data-cell="numeric">
              {String(index + 1).padStart(2, '0')}
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-xs text-text-primary">{row.label}</span>
              <span className="block truncate text-3xs text-text-muted">source · {row.source}</span>
            </span>
            <StateChip kind={laneStateKind(row.state)} detail={laneStateDetail(row.state)} />
          </li>
        ))}
      </ol>
    </Panel>
  );
}

/** Resolve the project overview read to the shared `ReadState` ladder. A
 * refused authorization is a blocked read, not a payload to render. */
export function overviewReadState(
  pending: boolean,
  result: EnvelopeResult<DeliveryOverviewV1> | undefined,
): ReadState<DeliveryOverviewV1> {
  if (pending) {
    return { kind: 'blocked', state: 'loading', detail: 'reading the project delivery overview' };
  }
  if (result === undefined) {
    return { kind: 'blocked', state: 'unknown', detail: 'no overview response recorded' };
  }
  if (result.outcome === 'transport') {
    return {
      kind: 'blocked',
      state: result.state,
      detail: result.detail ?? 'the project delivery overview could not be read',
    };
  }
  if (result.envelope.authorization.outcome !== 'authorized') {
    return {
      kind: 'blocked',
      state: result.envelope.authorization.outcome,
      detail: 'project delivery evidence was not disclosed',
    };
  }
  return { kind: 'ready', value: result.envelope.payload };
}
