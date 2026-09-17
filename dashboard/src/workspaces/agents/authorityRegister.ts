import type {
  AnalyticsDiagnosticsPayloadV1,
  AnalyticsSubagentTreePayloadV1,
  AnalyticsUsageSummaryV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import { readOutcomes, type AttemptFailureReading } from './failure.ts';
import type { AgentHandoffReading } from './handoff.ts';
import type { HandoffTokenReading } from './handoffTokens.ts';
import { ANALYTICS_EVENT_LIMIT } from './usage.ts';

/**
 * The five authorities the Agents workspace reads, each with its own state.
 *
 * They are never summed. A usage fold that answered, a hierarchy that was
 * truncated, a token frontier nobody asked for, a work graph that refused and
 * a diagnostics fold still loading are five facts, and a single "87% ready"
 * over them would be a number none of the five authorities reported. The
 * register prints them side by side and lets the reader do the reading.
 */
export interface AuthorityState {
  readonly id: 'usage' | 'hierarchy' | 'tokens' | 'work' | 'failure';
  readonly label: string;
  readonly kind: DomainStateKind;
  /** One short clause: the figure or the reason. */
  readonly detail: string;
  /** Where the reading came from, when the authority named it. */
  readonly source: string | null;
}

function transportState<T>(
  pending: boolean,
  result: EnvelopeResult<T> | undefined,
): { kind: DomainStateKind; detail: string } | null {
  if (pending) return { kind: 'loading', detail: 'reading' };
  if (result === undefined) return { kind: 'unknown', detail: 'no response recorded' };
  if (result.outcome === 'transport') {
    return { kind: result.state, detail: result.detail ?? 'could not be read' };
  }
  return null;
}

export function usageAuthority(
  pending: boolean,
  result: EnvelopeResult<AnalyticsUsageSummaryV1> | undefined,
): AuthorityState {
  const base = { id: 'usage', label: 'Usage' } as const;
  const blocked = transportState(pending, result);
  if (blocked) return { ...base, ...blocked, source: null };
  const envelope = result!.outcome === 'envelope' ? result!.envelope : null;
  const payload = envelope?.payload;
  if (!payload || payload.available === false) {
    return {
      ...base,
      kind: 'unavailable',
      detail: envelope?.coverage.omission_reasons[0] ?? 'analytics store unavailable',
      source: payload?.source ?? null,
    };
  }
  if (payload.event_count == null) {
    return {
      ...base,
      kind: 'partial',
      detail: `${payload.message_count.toLocaleString()} messages · event count not served`,
      source: payload.source,
    };
  }
  const capped = payload.event_count >= ANALYTICS_EVENT_LIMIT;
  return {
    ...base,
    kind: payload.event_count === 0 ? 'complete_zero_findings' : capped ? 'partial' : 'ready',
    detail: capped
      ? `${payload.event_count.toLocaleString()} events · endpoint cap`
      : `${payload.event_count.toLocaleString()} events`,
    source: payload.source,
  };
}

export function hierarchyAuthority(
  pending: boolean,
  result: EnvelopeResult<AnalyticsSubagentTreePayloadV1> | undefined,
): AuthorityState {
  const base = { id: 'hierarchy', label: 'Hierarchy' } as const;
  const blocked = transportState(pending, result);
  if (blocked) return { ...base, ...blocked, source: null };
  const envelope = result!.outcome === 'envelope' ? result!.envelope : null;
  const payload = envelope?.payload;
  if (!payload || payload.available === false) {
    return {
      ...base,
      kind: 'unavailable',
      detail: payload?.error ?? envelope?.coverage.omission_reasons[0] ?? 'session store unavailable',
      source: payload?.source ?? null,
    };
  }
  if (payload.nodes.length === 0) {
    return { ...base, kind: 'complete_zero_findings', detail: 'empty store · no session', source: payload.source };
  }
  const caveats = [
    payload.missing_parent_count > 0 ? `${payload.missing_parent_count} cut` : null,
    payload.cycle_count > 0 ? `${payload.cycle_count} ${payload.cycle_count === 1 ? 'cycle' : 'cycles'}` : null,
  ].filter(Boolean);
  const figure =
    payload.edge_count === 0
      ? `${payload.sessions_read.toLocaleString()} sessions · no delegation edge`
      : `${payload.edge_count.toLocaleString()} edges · ${payload.sessions_read.toLocaleString()} sessions`;
  return {
    ...base,
    kind: payload.truncated ? 'partial' : 'ready',
    detail: [figure, payload.truncated ? 'scan ceiling' : null, ...caveats].filter(Boolean).join(' · '),
    source: payload.source,
  };
}

export function tokenAuthority(reading: HandoffTokenReading): AuthorityState {
  const base = { id: 'tokens', label: 'Tokens', source: 'grant store' } as const;
  switch (reading.state) {
    case 'pending':
      return { ...base, kind: 'loading', detail: 'reading' };
    case 'unasked':
      return { ...base, kind: 'unknown', detail: 'unasked · no session named' };
    case 'refused':
      return { ...base, kind: reading.chip, detail: reading.detail };
    case 'read': {
      const total = reading.outstanding.length + reading.lapsed.length + reading.redeemed.length;
      return {
        ...base,
        kind: reading.truncated ? 'partial' : total === 0 ? 'complete_zero_findings' : 'ready',
        detail: `${reading.outstanding.length} open · ${reading.lapsed.length} lapsed · ${reading.redeemed.length} redeemed${reading.truncated ? ' · truncated' : ''}`,
      };
    }
    default: {
      const unhandled: never = reading;
      return unhandled;
    }
  }
}

export function workAuthority(
  handoffs: AgentHandoffReading,
  attempts: AttemptFailureReading,
): AuthorityState {
  const base = { id: 'work', label: 'Work', source: 'work.views' } as const;
  if (handoffs.state === 'pending') return { ...base, kind: 'loading', detail: 'reading' };
  if (handoffs.state === 'refused') return { ...base, kind: handoffs.chip, detail: handoffs.detail };
  const coverage =
    attempts.state === 'read'
      ? attempts.coverage === 'complete'
        ? `${attempts.attempts} attempts observed`
        : attempts.coverage === 'partial'
          ? `${attempts.unobserved} attempts unobserved`
          : 'attempts unobservable'
      : null;
  return {
    ...base,
    kind:
      attempts.state === 'read' && attempts.coverage !== 'complete'
        ? 'partial'
        : handoffs.handoffs.length === 0
          ? 'complete_zero_findings'
          : 'ready',
    detail: [
      `${handoffs.handoffs.length} ${handoffs.handoffs.length === 1 ? 'handoff' : 'handoffs'} · v${handoffs.graphVersion}`,
      coverage,
    ]
      .filter(Boolean)
      .join(' · '),
  };
}

export function failureAuthority(
  pending: boolean,
  result: EnvelopeResult<AnalyticsDiagnosticsPayloadV1> | undefined,
): AuthorityState {
  const base = { id: 'failure', label: 'Failure' } as const;
  const blocked = transportState(pending, result);
  if (blocked) return { ...base, ...blocked, source: null };
  const envelope = result!.outcome === 'envelope' ? result!.envelope : null;
  const payload = envelope?.payload;
  if (!payload || payload.available === false) {
    return {
      ...base,
      kind: 'unavailable',
      detail: envelope?.coverage.omission_reasons[0] ?? 'diagnostics unavailable',
      source: payload?.source ?? null,
    };
  }
  const outcomes = readOutcomes(payload.by_outcome);
  return {
    ...base,
    kind: payload.event_count === 0 ? 'complete_zero_findings' : 'ready',
    detail: [
      `${outcomes.failedTotal.toLocaleString()} failed of ${outcomes.counted.toLocaleString()} outcomes`,
      outcomes.unclassifiedTotal > 0 ? `${outcomes.unclassifiedTotal.toLocaleString()} unclassified` : null,
      `${payload.recent_events.length} on tape`,
    ]
      .filter(Boolean)
      .join(' · '),
    source: payload.source,
  };
}
