import { RefreshCw } from 'lucide-react';
import {
  assertNever,
  type DashboardAuthorizationV1,
  type DashboardCoverageV1,
  type DashboardEnvelopeV1,
  type DashboardFreshnessV1,
} from '../contracts/generated.ts';
import { EvidenceTruthStrip } from './EvidenceTruthStrip.tsx';
import { StateChip, type DomainStateKind } from './StateChip';

/**
 * The truth header every `DashboardEnvelopeV1` read carries: its domain state,
 * its coverage with denominator, its freshness, and — only when the server
 * returned one — a refresh control bound to the server's own legal-action
 * reference.
 *
 * The refresh button is deliberately conditional on that reference rather than
 * always drawn: a control the application did not offer is not a control, and
 * rendering one anyway would make the browser the authority on what operations
 * exist.
 */
export function EnvelopeTruth({
  envelope,
  refreshing,
  onRefresh,
}: {
  envelope: DashboardEnvelopeV1<unknown>;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  const { coverage, freshness } = envelope;
  const refresh = envelope.legal_actions.find((action) => action.kind === 'refresh')?.operation;
  const authorizationChip = authorizationState(envelope.authorization);
  return (
    <div className="flex flex-wrap items-center gap-3 px-4 pt-2">
      <StateChip kind={envelope.domain_state} />
      {/* The authorization outcome is a second, independent axis: a read can be
        * `partial` because a source was slow and separately `redacted` because
        * the identity behind it may not see the content. Folding one into the
        * other loses which of the two is actually blocking the reader. */}
      {authorizationChip ? (
        <StateChip kind={authorizationChip} detail="read authorization" />
      ) : null}
      <EvidenceTruthStrip
        coverage={toStripCoverage(coverage)}
        freshness={toStripFreshness(freshness)}
        // The omitted COUNT, not the number of reasons: two sentences can
        // explain forty omitted rows, and "2 omitted" would understate it.
        omissions={coverage.omitted ?? undefined}
      />
      {refresh ? (
        <button
          type="button"
          className="td-hit group ml-auto disabled:cursor-wait disabled:opacity-60"
          onClick={onRefresh}
          disabled={refreshing}
          title={refresh}
          data-operation={refresh}
        >
          <span className="inline-flex h-7 items-center gap-1.5 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 px-2.5 text-2xs font-medium text-text-secondary group-hover:text-text-primary">
            <RefreshCw aria-hidden size={12} className={refreshing ? 'animate-spin' : undefined} />
            {refreshing ? 'Refreshing' : 'Refresh'}
          </span>
        </button>
      ) : null}
    </div>
  );
}

/** The envelope's authorization outcome as its own domain state. `authorized`
 * has no chip — it is the ordinary case, and a badge on every read would make
 * the three that matter invisible. */
export function authorizationState(authorization: DashboardAuthorizationV1): DomainStateKind | null {
  switch (authorization.outcome) {
    case 'authorized':
      return null;
    case 'denied':
      return 'denied';
    case 'redacted':
      return 'redacted';
    case 'unauthorized':
      return 'unauthorized';
    default:
      return assertNever(authorization);
  }
}

/** Server omission reasons, verbatim. A coverage shortfall the server explained
 * is never reduced to the word "partial" on its own. */
export function OmissionReasons({ coverage }: { coverage: DashboardCoverageV1 }) {
  if (coverage.omission_reasons.length === 0) return null;
  return (
    <div className="mx-4 mt-2">
      <p className="text-3xs font-medium uppercase tracking-wide text-text-muted">
        Why this read is incomplete
      </p>
      <ul className="mt-1 space-y-1 text-2xs text-text-secondary">
        {coverage.omission_reasons.map((reason) => (
          <li key={reason}>{reason}</li>
        ))}
      </ul>
    </div>
  );
}

export function toStripCoverage(coverage: DashboardCoverageV1) {
  return {
    completeness: coverage.completeness,
    examined: coverage.examined,
    eligible: coverage.eligible,
  };
}

export function toStripFreshness(freshness: DashboardFreshnessV1) {
  return {
    state: freshness.state,
    observed_at:
      freshness.observed_at_micros != null
        ? new Date(freshness.observed_at_micros / 1000).toLocaleTimeString()
        : undefined,
  };
}
