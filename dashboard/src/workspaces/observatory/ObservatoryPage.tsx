import { useQuery } from '@tanstack/react-query';
import { fetchEnvelope } from '../../data/query/envelope.ts';
import {
  StorageTelemetryPayloadSchema,
  type StoreTelemetryEntry,
  type WireCoverage,
  type WireFreshness,
} from '../../contracts/wire.ts';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import { EvidenceTruthStrip } from '../../ui/EvidenceTruthStrip.tsx';
import { CapacityBar } from '../../ui/ActivityColumns.tsx';

/** Observatory landing (archetype 1): storage health from the real
 * /api/storage/telemetry envelope. Every state renders truthfully. */
export function ObservatoryPage() {
  const query = useQuery({
    queryKey: ['storage', 'telemetry'],
    queryFn: () => fetchEnvelope('/api/storage/telemetry', StorageTelemetryPayloadSchema),
    refetchInterval: 30_000,
  });

  if (query.isPending) {
    return (
      <CenteredState kind="loading" detail="requesting storage telemetry" />
    );
  }
  const result = query.data;
  if (!result) return <CenteredState kind="unknown" detail="no response recorded" />;
  if (result.outcome === 'transport') {
    return <CenteredState kind={result.state as DomainStateKind} detail={result.detail ?? 'daemon unreachable'} />;
  }

  const envelope = result.envelope;
  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
        <h1 className="text-sm font-semibold tracking-tight">Observatory</h1>
        <StateChip kind={envelope.domain_state as DomainStateKind} />
        <EvidenceTruthStrip
          coverage={toStripCoverage(envelope.coverage)}
          freshness={toStripFreshness(envelope.freshness)}
        />
      </div>
      <OverviewGrid className="overflow-auto">
        {envelope.payload.stores.map((store) => (
          <StoreCard key={store.store} entry={store} />
        ))}
      </OverviewGrid>
      <p className="border-t border-edge-subtle px-4 py-2 text-2xs text-text-muted">
        budgets: {envelope.payload.budget_note} · growth: {envelope.payload.growth_note}
      </p>
    </div>
  );
}

function StoreCard({ entry }: { entry: StoreTelemetryEntry }) {
  const observed = entry.read.kind === 'observed';
  return (
    <OverviewCard title={entry.store}>
      <div className="flex flex-col gap-1.5">
        <div className="flex items-center gap-2">
          <StateChip kind={readKindToState(entry.read.kind)} />
          <span className="text-2xs text-text-muted">{entry.role}</span>
        </div>
        {observed ? (
          <>
          <CapacityBar usedBytes={entry.total_bytes} freeBytes={entry.free_bytes} />
          <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs tabular">
            <dt className="text-text-muted">size</dt>
            <dd data-cell="numeric">{formatBytes(entry.total_bytes)}</dd>
            <dt className="text-text-muted">free pages</dt>
            <dd data-cell="numeric">{formatBytes(entry.free_bytes)}</dd>
            <dt className="text-text-muted">free ratio</dt>
            <dd data-cell="numeric">
              {entry.free_page_ratio != null ? `${(entry.free_page_ratio * 100).toFixed(1)}%` : '—'}
            </dd>
          </dl>
          </>
        ) : (
          <p className="text-xs text-text-muted">telemetry not observed for this store</p>
        )}
        <p className="truncate font-mono text-2xs text-text-muted" title={entry.path}>
          {entry.path}
        </p>
      </div>
    </OverviewCard>
  );
}

function CenteredState({ kind, detail }: { kind: DomainStateKind; detail?: string }) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-8">
      <h1 className="text-lg font-semibold tracking-tight">Observatory</h1>
      <StateChip kind={kind} detail={detail} />
    </div>
  );
}

function readKindToState(kind: string): DomainStateKind {
  switch (kind) {
    case 'observed':
      return 'ready';
    case 'unsupported':
      return 'unknown';
    case 'denied':
      return 'denied';
    default:
      return 'unknown';
  }
}

function formatBytes(bytes: number | null): string {
  if (bytes == null) return '—';
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${bytes} B`;
}

function toStripCoverage(coverage: WireCoverage) {
  return { examined: coverage.examined, eligible: coverage.eligible } as never;
}

function toStripFreshness(freshness: WireFreshness) {
  return {
    observed_at:
      freshness.observed_at_micros != null
        ? new Date(freshness.observed_at_micros / 1000).toLocaleTimeString()
        : undefined,
  } as never;
}
