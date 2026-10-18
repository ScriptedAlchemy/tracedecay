import { useQuery } from '@tanstack/react-query';
import { useMemo } from 'react';
import {
  AnalyticsDiagnosticsPayloadV1Schema,
  AnalyticsHintsPayloadV1Schema,
  CodeIndexFreshnessPayloadV1Schema,
  StorageTelemetryPayloadV1Schema,
  type AnalyticsDiagnosticsPayloadV1,
  type AnalyticsHintsPayloadV1,
  type CodeIndexFreshnessPayloadV1,
  type DoctorFindingsPayloadV1,
  type ExecutionTopologyMetricsV1,
  type ObservatoryReadModelV1,
  type StorageTelemetryPayloadV1,
} from '../../contracts/generated.ts';
import { doctorFindingsQueryKey, fetchDoctorFindings } from '../../data/query/doctor.ts';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { useStorageFindings } from '../../data/query/storageFindings.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import type { WorkResult } from '../work/workApi.ts';
import { useWorkTopologyMetrics } from '../work/workViewsQueries.ts';
import type { ObservatoryAccountingReads } from './accountingReads.ts';
import { hasActiveCodeIndexBuild } from './CodeIndexPipeline.tsx';
import {
  adoptionSummary,
  analyticsSummary,
  budgetsSummary,
  doctorSummary,
  findingsSummary,
  hooksSummary,
  observationsSummary,
  pipelineSummary,
  retrievalSummary,
  telemetrySummary,
  topologySummary,
  type EvidenceRead,
  type EvidenceSourceId,
  type EvidenceSummary,
} from './evidence.ts';
import { useObservatoryReadModel } from './observatoryReadModel.ts';

/**
 * Every read the Observatory pays for, requested once.
 *
 * The panel grid, the timeline, the inspector, and the exact-evidence region
 * all consume these same results, so no two regions can sit under different
 * watermarks of one authority. Section components that keep their own hooks
 * (`HookHints`, `DoctorInspector`, the canonical read-model sections) spell
 * the same query keys as this file, so React Query holds one entry per route.
 */
export interface ObservatoryReads {
  scopeKey: string;
  telemetry: EvidenceRead<StorageTelemetryPayloadV1>;
  freshness: EvidenceRead<CodeIndexFreshnessPayloadV1>;
  findings: EvidenceRead<DoctorFindingsPayloadV1>;
  doctor: EvidenceRead<DoctorFindingsPayloadV1>;
  observatory: EvidenceRead<ObservatoryReadModelV1>;
  diagnostics: EvidenceRead<AnalyticsDiagnosticsPayloadV1>;
  hints: EvidenceRead<AnalyticsHintsPayloadV1>;
  topology: {
    pending: boolean;
    result: WorkResult<ExecutionTopologyMetricsV1> | undefined;
    updatedAtMs: number;
  };
  accounting: ObservatoryAccountingReads;
  summaries: Record<EvidenceSourceId, EvidenceSummary>;
  /** Whether the authority behind a source is currently being re-read. */
  refreshing: (id: EvidenceSourceId) => boolean;
  /** Re-read the authority behind a source. Only bound where the daemon
   * declared a refresh action; the inspector checks that before drawing. */
  refresh: (id: EvidenceSourceId) => void;
}

function evidenceRead<T>(query: {
  isPending: boolean;
  data: EnvelopeResult<T> | undefined;
  dataUpdatedAt: number;
}): EvidenceRead<T> {
  return { pending: query.isPending, result: query.data, updatedAtMs: query.dataUpdatedAt };
}

type RawReads = Pick<
  ObservatoryReads,
  'telemetry' | 'freshness' | 'findings' | 'doctor' | 'observatory' | 'diagnostics' | 'hints' | 'topology'
>;

function deriveSummaries(reads: RawReads): Record<EvidenceSourceId, EvidenceSummary> {
  return {
    observations: observationsSummary(reads.observatory),
    doctor: doctorSummary(reads.doctor),
    adoption: adoptionSummary(reads.observatory),
    retrieval: retrievalSummary(reads.observatory),
    pipeline: pipelineSummary(reads.freshness),
    hooks: hooksSummary(reads.hints),
    budgets: budgetsSummary(reads.observatory),
    topology: topologySummary(reads.topology),
    analytics: analyticsSummary(reads.observatory),
    telemetry: telemetrySummary(reads.telemetry),
    findings: findingsSummary(reads.findings),
  };
}

export function useObservatoryReads(): ObservatoryReads {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);

  const telemetry = useQuery({
    queryKey: ['storage', 'telemetry', key],
    queryFn: () =>
      fetchEnvelope(scopedUrl(scope, '/api/storage/telemetry'), StorageTelemetryPayloadV1Schema),
    refetchInterval: 30_000,
  });
  const freshness = useQuery({
    queryKey: ['code-index', 'freshness', key],
    queryFn: () =>
      fetchEnvelope(scopedUrl(scope, '/api/code-index/freshness'), CodeIndexFreshnessPayloadV1Schema),
    refetchInterval: (query) => (hasActiveCodeIndexBuild(query.state.data) ? 1_000 : 30_000),
  });
  // Shared with the nav rail's Doctor dot, through the module that owns the
  // key, the route, and the poll: one entry, one period, one contract.
  const findings = useStorageFindings();
  const doctor = useQuery({
    queryKey: doctorFindingsQueryKey(scope),
    queryFn: () => fetchDoctorFindings(scope),
    refetchInterval: 30_000,
  });
  const observatory = useObservatoryReadModel();
  const diagnostics = useEnvelope(
    ['observatory', 'accounting-diagnostics'],
    '/api/plugins/analytics/diagnostics',
    AnalyticsDiagnosticsPayloadV1Schema,
    { staleTime: 30_000 },
  );
  const hints = useEnvelope(
    ['analytics', 'hints'],
    '/api/plugins/analytics/hints',
    AnalyticsHintsPayloadV1Schema,
  );
  const topology = useWorkTopologyMetrics(true);

  const reads = {
    telemetry: evidenceRead(telemetry),
    freshness: evidenceRead(freshness),
    findings: evidenceRead(findings),
    doctor: evidenceRead(doctor),
    observatory: evidenceRead(observatory),
    diagnostics: evidenceRead(diagnostics),
    hints: evidenceRead(hints),
    topology: {
      pending: topology.isPending,
      result: topology.data,
      updatedAtMs: topology.dataUpdatedAt,
    },
  };

  // Summaries are derived from the reads' identity-stable fields so a hover
  // re-render of the page does not hand every panel a fresh object.
  const summaries = useMemo<Record<EvidenceSourceId, EvidenceSummary>>(
    () => deriveSummaries(reads),
    [
      reads.telemetry.pending,
      reads.telemetry.result,
      reads.telemetry.updatedAtMs,
      reads.freshness.pending,
      reads.freshness.result,
      reads.freshness.updatedAtMs,
      reads.findings.pending,
      reads.findings.result,
      reads.findings.updatedAtMs,
      reads.doctor.pending,
      reads.doctor.result,
      reads.doctor.updatedAtMs,
      reads.observatory.pending,
      reads.observatory.result,
      reads.observatory.updatedAtMs,
      reads.hints.pending,
      reads.hints.result,
      reads.hints.updatedAtMs,
      reads.topology.pending,
      reads.topology.result,
      reads.topology.updatedAtMs,
    ],
  );

  const queryFor = (id: EvidenceSourceId) => {
    switch (id) {
      case 'observations':
      case 'adoption':
      case 'retrieval':
      case 'budgets':
      case 'analytics':
        return observatory;
      case 'doctor':
        return doctor;
      case 'pipeline':
        return freshness;
      case 'hooks':
        return hints;
      case 'topology':
        return topology;
      case 'telemetry':
        return telemetry;
      case 'findings':
        return findings;
      default: {
        const unhandled: never = id;
        return unhandled;
      }
    }
  };

  return {
    scopeKey: key,
    ...reads,
    accounting: {
      observatory: {
        result: observatory.data,
        pending: observatory.isPending,
        refreshing: observatory.isFetching,
        refresh: () => void observatory.refetch(),
      },
      diagnostics: {
        result: diagnostics.data,
        pending: diagnostics.isPending,
        refreshing: diagnostics.isFetching,
        refresh: () => void diagnostics.refetch(),
      },
    },
    summaries,
    refreshing: (id) => queryFor(id).isFetching,
    refresh: (id) => void queryFor(id).refetch(),
  };
}
