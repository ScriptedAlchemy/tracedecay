import { Corners } from '../../ui/instrument.tsx';
import { EnvelopeSection } from '../../ui/ReadSection.tsx';
import { AdoptionCoverage } from './AdoptionCoverage.tsx';
import { AdoptionOutcomes } from './AdoptionOutcomes.tsx';
import { AnalyticsControls } from './AnalyticsControls.tsx';
import { CanonicalObservations } from './CanonicalObservations.tsx';
import { CloneIndexStatus } from './CloneIndexStatus.tsx';
import { CodeIndexPipeline } from './CodeIndexPipeline.tsx';
import { DoctorInspector } from './DoctorInspector.tsx';
import { ExecutionTopologyMetrics } from './ExecutionTopologyMetrics.tsx';
import { HookHints } from './HookHints.tsx';
import { PerformanceBudgets } from './PerformanceBudgets.tsx';
import { PerformanceComparisons } from './PerformanceComparisons.tsx';
import { RejectedArguments } from './RejectedArguments.tsx';
import { RetrievalQuality } from './RetrievalQuality.tsx';
import { FindingsReadModel } from './StorageFindings.tsx';
import { TelemetryReadModel } from './StorageTelemetry.tsx';
import type { EvidenceSourceId } from './evidence.ts';
import { SOURCE_IDENTITY } from './evidence.ts';
import { EXACT_EVIDENCE_ID } from './exactEvidenceId.ts';
import type { ObservatoryReads } from './useObservatoryReads.ts';


/**
 * The exact fallback for the selected authority: its full read model, with
 * every source-qualified table, timestamp, coverage statement, and typed
 * state, rendered by the component that owns that read. The overview panel is
 * the summary; this is the evidence it summarises.
 */
export function ExactEvidence({
  id,
  reads,
}: {
  id: EvidenceSourceId;
  reads: ObservatoryReads;
}) {
  const identity = SOURCE_IDENTITY[id];
  return (
    <section
      id={EXACT_EVIDENCE_ID}
      aria-label={`Exact evidence · ${identity.title}`}
      data-exact-evidence={id}
      className="relative mt-2 border border-edge-subtle bg-surface-0"
    >
      <Corners />
      <header className="flex h-8 shrink-0 items-center gap-2 border-b border-edge-subtle px-3">
        <span className="td-legend text-text-secondary">Exact evidence</span>
        <span className="td-title truncate text-text-primary">{identity.title}</span>
        <span aria-hidden className="td-rule" />
        <span className="td-legend truncate font-mono normal-case tracking-normal">{identity.route}</span>
      </header>
      <ExactEvidenceBody id={id} reads={reads} />
    </section>
  );
}

function ExactEvidenceBody({ id, reads }: { id: EvidenceSourceId; reads: ObservatoryReads }) {
  switch (id) {
    case 'observations':
      return <CanonicalObservations />;
    case 'doctor':
      return <DoctorInspector />;
    case 'adoption':
      return (
        <>
          <AdoptionCoverage reads={reads.accounting} />
          <AdoptionOutcomes reads={reads.accounting} />
        </>
      );
    case 'retrieval':
      return <RetrievalQuality reads={reads.accounting} />;
    case 'pipeline':
      return (
        <div className="pb-3">
          <CodeIndexPipeline
            result={reads.freshness.result}
            pending={reads.freshness.pending}
            scopeKey={reads.scopeKey}
          />
          <CloneIndexStatus result={reads.freshness.result} pending={reads.freshness.pending} />
        </div>
      );
    case 'hooks':
      return (
        <>
          <HookHints />
          <RejectedArguments reads={reads.accounting} />
        </>
      );
    case 'budgets':
      return (
        <>
          <PerformanceBudgets />
          <PerformanceComparisons />
        </>
      );
    case 'topology':
      return (
        <div className="p-3">
          <ExecutionTopologyMetrics />
        </div>
      );
    case 'analytics':
      return <AnalyticsControls reads={reads.accounting} />;
    case 'telemetry':
      return (
        <EnvelopeSection
          title="Store telemetry"
          result={reads.telemetry.result}
          pending={reads.telemetry.pending}
          loadingDetail="requesting store telemetry"
        >
          {(envelope) => (
            <TelemetryReadModel
              envelope={envelope}
              refreshing={reads.refreshing('telemetry')}
              onRefresh={() => reads.refresh('telemetry')}
            />
          )}
        </EnvelopeSection>
      );
    case 'findings':
      return (
        <EnvelopeSection
          title="Doctor storage findings"
          result={reads.findings.result}
          pending={reads.findings.pending}
          loadingDetail="requesting doctor storage findings"
        >
          {(envelope) => (
            <FindingsReadModel
              envelope={envelope}
              refreshing={reads.refreshing('findings')}
              onRefresh={() => reads.refresh('findings')}
            />
          )}
        </EnvelopeSection>
      );
    default: {
      const unhandled: never = id;
      return unhandled;
    }
  }
}
