import type {
  DoctorEvidenceState,
  DoctorStorageFindingKind,
  WireLegalActionRef,
} from '../../contracts/wire.ts';

const FINDING_LABELS: Record<DoctorStorageFindingKind, string> = {
  over_budget_store: 'Over-budget stores',
  orphan_store: 'Orphan stores',
  stale_branch_dbs: 'Stale branch databases',
  incident_debris_present: 'Incident debris',
  retention_backlog: 'Retention backlog',
};

const EVIDENCE_PRESENTATION: Record<
  DoctorEvidenceState,
  { label: string; tokenClass: string; dotClass: string }
> = {
  unsupported: {
    label: 'Unsupported',
    tokenClass: 'text-state-unsupported-schema',
    dotClass: 'bg-state-unsupported-schema',
  },
  absent: { label: 'Absent', tokenClass: 'text-state-unknown', dotClass: 'bg-state-unknown' },
  stale: { label: 'Stale', tokenClass: 'text-state-stale', dotClass: 'bg-state-stale' },
  degraded: { label: 'Degraded', tokenClass: 'text-state-error', dotClass: 'bg-state-error' },
  partial: { label: 'Partial', tokenClass: 'text-state-partial', dotClass: 'bg-state-partial' },
  unknown: { label: 'Unknown', tokenClass: 'text-state-unknown', dotClass: 'bg-state-unknown' },
  denied: { label: 'Denied', tokenClass: 'text-state-denied', dotClass: 'bg-state-denied' },
  healthy_complete_coverage: {
    label: 'Healthy · complete coverage',
    tokenClass: 'text-state-ready',
    dotClass: 'bg-state-ready',
  },
};

export function storageFindingLabel(kind: DoctorStorageFindingKind): string {
  return FINDING_LABELS[kind];
}

export function doctorEvidencePresentation(state: DoctorEvidenceState) {
  return EVIDENCE_PRESENTATION[state];
}

export function refreshOperation(actions: WireLegalActionRef[]): string | undefined {
  return actions.find((action) => action.kind === 'refresh')?.operation;
}
