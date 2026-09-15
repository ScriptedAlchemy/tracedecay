import { useQuery } from '@tanstack/react-query';
import {
  CodeIndexFreshnessPayloadV1Schema,
  StorageTelemetryPayloadV1Schema,
  type DashboardDomainStateV1,
  type DoctorStorageFindingKindV1,
  type StorageFindingsPayloadV1,
} from '../../contracts/generated.ts';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { useStorageFindings } from '../../data/query/storageFindings.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { cn } from '../../ui/cn';

/**
 * The Observatory's segmented subsystem header: six numbered registers across
 * the top of the channel — STORES / INDEX / DOCTOR / BUDGET / ORPHANS /
 * DEBRIS — each stamped with the current reading of a source this page
 * already pays for. The grammar is the segmented mission header (numbered
 * cells, engraved label, stamp word beside a lamp); the readings are real:
 *
 *   STORES   the store-telemetry envelope's own domain state
 *   INDEX    the code-index freshness envelope's domain state
 *   DOCTOR   the canonical Doctor storage projection's domain state
 *   BUDGET   the over-budget producer: AMBER ALERT when a served finding is
 *            degraded or stale, otherwise the producer's source coverage
 *   ORPHANS  the orphan-store producer, same rule
 *   DEBRIS   the incident-debris producer, same rule
 *
 * A stamp is never invented: a read still in flight says READING, a transport
 * failure says its transport state, and an unsupported producer says
 * UNSUPPORTED. Amber is reserved for an actually-observed problem finding —
 * alert is a report, not a decoration.
 */

type SegmentTone = 'ok' | 'alert' | 'attention';

interface Segment {
  code: string;
  label: string;
  word: string;
  tone: SegmentTone;
}

const TONE_LAMP: Record<SegmentTone, string> = {
  ok: 'bg-state-ready',
  alert: 'bg-alert',
  attention: 'bg-state-unknown',
};

const TONE_WORD: Record<SegmentTone, string> = {
  ok: 'text-text-secondary',
  alert: 'text-alert',
  attention: 'text-text-muted',
};

/** One envelope-backed segment: the domain state, said as a stamp word. */
function envelopeSegment(
  code: string,
  label: string,
  pending: boolean,
  result: EnvelopeResult<unknown> | undefined,
): Segment {
  if (pending) return { code, label, word: 'reading', tone: 'attention' };
  if (!result) return { code, label, word: 'unknown', tone: 'attention' };
  const state: DashboardDomainStateV1 =
    result.outcome === 'transport' ? result.state : result.envelope.domain_state;
  return { code, label, ...domainStamp(state) };
}

function domainStamp(state: DashboardDomainStateV1): { word: string; tone: SegmentTone } {
  switch (state) {
    case 'ready':
    case 'complete_zero_findings':
      return { word: 'ok', tone: 'ok' };
    case 'error':
    case 'denied':
    case 'unauthorized':
    case 'conflicting':
      return { word: state.replaceAll('_', ' '), tone: 'alert' };
    default:
      return { word: state.replaceAll('_', ' '), tone: 'attention' };
  }
}

/** One producer-backed segment off the storage-findings projection. */
function producerSegment(
  code: string,
  label: string,
  pending: boolean,
  result: EnvelopeResult<StorageFindingsPayloadV1> | undefined,
  kind: DoctorStorageFindingKindV1,
): Segment {
  if (pending) return { code, label, word: 'reading', tone: 'attention' };
  if (!result) return { code, label, word: 'unknown', tone: 'attention' };
  if (result.outcome === 'transport') {
    return { code, label, ...domainStamp(result.state) };
  }
  const payload = result.envelope.payload;
  // A served finding in a problem state is the one thing that earns amber.
  const problems = payload.entries.filter(
    (entry) =>
      entry.storage_kind === kind &&
      (entry.finding.state === 'degraded' || entry.finding.state === 'stale'),
  ).length;
  if (problems > 0) {
    return { code, label, word: `alert · ${problems}`, tone: 'alert' };
  }
  const status = payload.kind_statuses.find((entry) => entry.kind === kind);
  if (!status) return { code, label, word: 'unknown', tone: 'attention' };
  switch (status.state) {
    case 'real':
      return { code, label, word: 'ok', tone: 'ok' };
    case 'partial':
      return { code, label, word: 'partial', tone: 'attention' };
    case 'unsupported':
      return { code, label, word: 'unsupported', tone: 'attention' };
    default:
      return { code, label, word: 'unknown', tone: 'attention' };
  }
}

export function SystemsHeader() {
  const scope = useScope((s) => s.scope);
  const key = scopeKey(scope);
  // The same cache entries the Diagnosis wing reads (same keys, same routes),
  // so this header costs no extra requests while that wing is open.
  const telemetry = useQuery({
    queryKey: ['storage', 'telemetry', key],
    queryFn: () =>
      fetchEnvelope(scopedUrl(scope, '/api/storage/telemetry'), StorageTelemetryPayloadV1Schema),
    refetchInterval: 30_000,
  });
  const freshness = useQuery({
    queryKey: ['code-index', 'freshness', key],
    queryFn: () =>
      fetchEnvelope(
        scopedUrl(scope, '/api/code-index/freshness'),
        CodeIndexFreshnessPayloadV1Schema,
      ),
    refetchInterval: 30_000,
  });
  const findings = useStorageFindings();

  const segments: Segment[] = [
    envelopeSegment('01', 'Stores', telemetry.isPending, telemetry.data),
    envelopeSegment('02', 'Index', freshness.isPending, freshness.data),
    envelopeSegment('03', 'Doctor', findings.isPending, findings.data),
    producerSegment('04', 'Budget', findings.isPending, findings.data, 'over_budget_store'),
    producerSegment('05', 'Orphans', findings.isPending, findings.data, 'orphan_store'),
    producerSegment('06', 'Debris', findings.isPending, findings.data, 'incident_debris_present'),
  ];

  return (
    <ul
      aria-label="Subsystem status"
      className="flex flex-wrap gap-1.5 border-b border-edge-subtle bg-surface-0 px-4 py-1.5"
    >
      {segments.map((segment) => (
        <li
          key={segment.code}
          data-subsystem={segment.label.toLowerCase()}
          data-subsystem-tone={segment.tone}
          className={cn(
            'flex min-w-0 items-center gap-2 border px-2.5 py-1.5',
            segment.tone === 'alert' ? 'border-alert/60' : 'border-edge-subtle',
            'bg-surface-1',
          )}
        >
          <span aria-hidden className="td-value text-3xs text-text-muted" data-cell="numeric">
            {segment.code}
          </span>
          <span className="td-legend">{segment.label}</span>
          <span aria-hidden className={cn('size-1.5 shrink-0', TONE_LAMP[segment.tone])} />
          <span className={cn('td-legend', TONE_WORD[segment.tone])}>{segment.word}</span>
        </li>
      ))}
    </ul>
  );
}
