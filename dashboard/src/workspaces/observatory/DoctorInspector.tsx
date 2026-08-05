import { useQuery } from '@tanstack/react-query';
import type { LucideIcon } from 'lucide-react';
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  CircleSlash,
  Clock,
  FileSearch,
  HelpCircle,
  ShieldX,
  XCircle,
} from 'lucide-react';
import {
  assertNever,
  type DoctorEvidenceStateV1,
  type DoctorFindingsPayloadV1,
  type DoctorReportCoverageV1,
  type DoctorReportEntryV1,
} from '../../contracts/generated.ts';
import { doctorFindingsQueryKey, fetchDoctorFindings } from '../../data/query/doctor.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { useScope } from '../../data/scope/store.ts';
import { EnvelopeTruth } from '../../ui/EnvelopeTruth.tsx';
import { EvidenceTruthStrip } from '../../ui/EvidenceTruthStrip.tsx';
import { ReadModelState, envelopeReadState } from '../../ui/ReadSection.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { cn } from '../../ui/cn.ts';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid.tsx';
import { doctorEvidencePresentation, doctorFamilyLabel } from './doctorModel.ts';

/** Canonical, read-only Doctor diagnostics for the selected project scope. */
export function DoctorInspector() {
  const scope = useScope((state) => state.scope);
  const findings = useQuery({
    queryKey: doctorFindingsQueryKey(scope),
    queryFn: () => fetchDoctorFindings(scope),
    refetchInterval: 30_000,
  });

  return (
    <section className="border-b border-edge-subtle" aria-label="Doctor diagnosis">
      <div className="flex flex-wrap items-center gap-2 px-4 pt-4">
        <Activity aria-hidden size={14} className="text-accent" />
        <h2 className="text-sm font-semibold tracking-tight">Doctor diagnosis</h2>
        <span className="text-2xs text-text-muted">
          canonical evidence and typed diagnostics
        </span>
      </div>
      <DoctorFindings
        result={findings.data}
        pending={findings.isPending}
        refreshing={findings.isFetching}
        onRefresh={() => void findings.refetch()}
      />
    </section>
  );
}

function DoctorFindings({
  result,
  pending,
  refreshing,
  onRefresh,
}: {
  result: EnvelopeResult<DoctorFindingsPayloadV1> | undefined;
  pending: boolean;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  const read = envelopeReadState(pending, result, {
    loading: 'requesting canonical Doctor findings',
    unknown: 'no Doctor response recorded',
  });
  if (read.kind === 'blocked') {
    return <ReadModelState kind={read.state} detail={read.detail} />;
  }

  const envelope = read.value;
  return (
    <>
      <EnvelopeTruth envelope={envelope} refreshing={refreshing} onRefresh={onRefresh} />
      <DoctorReportCoverageGaps coverage={envelope.payload.report_coverage} />
      {envelope.payload.entries.length === 0 ? (
        <ReadModelState kind={envelope.domain_state} detail={envelope.payload.note} />
      ) : (
        <OverviewGrid>
          {envelope.payload.entries.map((entry, index) => (
            <FindingCard
              key={`${entry.finding.family}:${entry.storage_kind ?? 'general'}:${index}`}
              entry={entry}
            />
          ))}
        </OverviewGrid>
      )}
      <p className="border-t border-edge-subtle px-4 py-2 text-2xs text-text-muted">
        {envelope.payload.note}
      </p>
    </>
  );
}

function DoctorReportCoverageGaps({
  coverage,
}: {
  coverage: DoctorReportCoverageV1 | null;
}) {
  const unavailable =
    coverage?.families.filter((family) => family.consultation.status === 'unavailable') ??
    [];
  if (unavailable.length === 0) return null;
  return (
    <div
      className="mx-4 mt-2 flex flex-wrap gap-2 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 p-2"
      aria-label="Doctor source coverage gaps"
    >
      {unavailable.map(({ family, consultation }) => {
        if (consultation.status !== 'unavailable') return null;
        return (
          <StateChip
            key={family}
            kind={consultationState(consultation.reason)}
            detail={`${doctorFamilyLabel(family)} ${consultation.reason}`}
          />
        );
      })}
    </div>
  );
}

function consultationState(
  reason: 'unwired' | 'unsupported' | 'absent' | 'denied' | 'unknown',
): DomainStateKind {
  switch (reason) {
    case 'denied':
      return 'denied';
    case 'unsupported':
    case 'unwired':
      return 'unsupported';
    case 'absent':
    case 'unknown':
      return 'unknown';
    default:
      return assertNever(reason);
  }
}

function FindingCard({ entry }: { entry: DoctorReportEntryV1 }) {
  const { finding } = entry;
  return (
    <OverviewCard title={doctorFamilyLabel(finding.family)}>
      <div className="flex flex-col gap-2">
        <EvidenceBadge state={finding.state} />
        <EvidenceTruthStrip
          coverage={{ completeness: finding.coverage.completeness }}
          citations={finding.evidence.length}
        />
        <p className="text-xs text-text-secondary">{finding.coverage.statement}</p>
        <FindingEvidence entry={entry} />
      </div>
    </OverviewCard>
  );
}

const EVIDENCE_ICON: Record<DoctorEvidenceStateV1, LucideIcon> = {
  unsupported: CircleSlash,
  absent: HelpCircle,
  stale: Clock,
  degraded: XCircle,
  partial: AlertTriangle,
  unknown: HelpCircle,
  denied: ShieldX,
  healthy_complete_coverage: CheckCircle2,
};

function EvidenceBadge({ state }: { state: DoctorEvidenceStateV1 }) {
  const evidence = doctorEvidencePresentation(state);
  const Icon = EVIDENCE_ICON[state];
  return (
    <span
      className="relative inline-flex w-fit items-center gap-1.5 border border-edge-subtle bg-surface-2 py-[3px] pl-2.5 pr-2 text-2xs font-medium"
      data-evidence-state={state}
    >
      <span
        aria-hidden
        className={cn('absolute inset-y-0 left-0 w-[2px]', evidence.dotClass)}
      />
      <Icon aria-hidden size={11} className={evidence.tokenClass} />
      <span className="text-text-secondary">{evidence.label}</span>
    </span>
  );
}

function FindingEvidence({ entry }: { entry: DoctorReportEntryV1 }) {
  return (
    <div>
      <p className="text-2xs font-medium uppercase tracking-wide text-text-muted">
        Evidence
      </p>
      <ul className="mt-1 space-y-1">
        {entry.finding.evidence.map((evidence) => (
          <li
            key={`${evidence.family}:${evidence.reference}`}
            className="flex items-start gap-1.5 text-2xs text-text-secondary"
          >
            <FileSearch aria-hidden size={11} className="mt-0.5 shrink-0 text-text-muted" />
            <span className="font-mono">{evidence.reference}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
