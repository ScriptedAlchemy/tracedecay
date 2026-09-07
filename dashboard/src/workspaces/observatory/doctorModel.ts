import type {
  DoctorEvidenceStateV1,
  DoctorFindingFamilyV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

const FAMILY_LABELS: Record<DoctorFindingFamilyV1, string> = {
  advisory: 'Advisory',
  configuration: 'Configuration',
  storage_runtime: 'Storage runtime',
  storage: 'Storage',
  language_server: 'Language server',
  semantic_index: 'Semantic index',
  observability: 'Observability',
};

export interface DoctorEvidencePresentation {
  label: string;
  tokenClass: string;
  dotClass: string;
  domainState: DomainStateKind;
}

const EVIDENCE_TOKENS: Record<
  DoctorEvidenceStateV1,
  { label: string; tokenClass: string; domainState: DomainStateKind }
> = {
  unsupported: {
    label: 'Unsupported',
    tokenClass: 'text-state-unsupported-schema',
    domainState: 'unsupported',
  },
  absent: { label: 'Absent', tokenClass: 'text-state-unknown', domainState: 'unknown' },
  stale: { label: 'Stale', tokenClass: 'text-state-stale', domainState: 'stale' },
  degraded: { label: 'Degraded', tokenClass: 'text-state-error', domainState: 'error' },
  partial: { label: 'Partial', tokenClass: 'text-state-partial', domainState: 'partial' },
  unknown: { label: 'Unknown', tokenClass: 'text-state-unknown', domainState: 'unknown' },
  denied: { label: 'Denied', tokenClass: 'text-state-denied', domainState: 'denied' },
  healthy_complete_coverage: {
    label: 'Healthy · complete coverage',
    tokenClass: 'text-state-ready',
    domainState: 'ready',
  },
};

export function doctorFamilyLabel(family: DoctorFindingFamilyV1): string {
  return FAMILY_LABELS[family];
}

export function doctorEvidencePresentation(
  state: DoctorEvidenceStateV1,
): DoctorEvidencePresentation {
  const tokens = EVIDENCE_TOKENS[state];
  return { ...tokens, dotClass: tokens.tokenClass.replace('text-', 'bg-') };
}
