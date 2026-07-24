import type {
  DoctorEvidenceState,
  DoctorFindingFamily,
  DoctorRemediationAuthorityScope,
  DoctorRemediationDescriptor,
  DoctorRemediationOperation,
  DoctorReportEntry,
  WireScope,
  WireLegalActionRef,
} from '../../contracts/wire.ts';
import {
  DoctorRemediationAuthorityScopeSchema,
  ScopeSchema,
} from '../../contracts/wire.ts';
import { z } from 'zod';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

const ACTIVE_OPERATION_KEY = 'tracedecay.doctor.active-operation.v2';
const ActiveDoctorOperationSchema = z.object({
  schema_revision: z.literal(2),
  operation_id: z.string(),
  transport_scope: ScopeSchema,
  authority_scope: DoctorRemediationAuthorityScopeSchema,
});

export type ActiveDoctorOperation = z.infer<typeof ActiveDoctorOperationSchema>;

const FAMILY_LABELS: Record<DoctorFindingFamily, string> = {
  advisory: 'Advisory',
  configuration: 'Configuration',
  storage_runtime: 'Storage runtime',
  storage: 'Storage',
  language_server: 'Language server',
  semantic_index: 'Semantic index',
  observability: 'Observability',
};

const EVIDENCE_PRESENTATION: Record<
  DoctorEvidenceState,
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

export function doctorFamilyLabel(family: DoctorFindingFamily): string {
  return FAMILY_LABELS[family];
}

export function doctorEvidencePresentation(state: DoctorEvidenceState) {
  return EVIDENCE_PRESENTATION[state];
}

export function remediationForEntry(
  entry: DoctorReportEntry,
  descriptors: DoctorRemediationDescriptor[],
): DoctorRemediationDescriptor | undefined {
  const operation = entry.finding.remediation?.owning_operation;
  return operation
    ? descriptors.find((descriptor) => descriptor.operation === operation)
    : undefined;
}

export function availableRemediationActions(
  descriptor: DoctorRemediationDescriptor,
  legalActions: WireLegalActionRef[],
): { canPreview: boolean; canApply: boolean } {
  const exactKinds = new Set(
    legalActions
      .filter((action) => action.operation === descriptor.operation)
      .map((action) => action.kind),
  );
  return {
    canPreview: descriptor.preview_available && exactKinds.has('request_dry_run'),
    canApply: exactKinds.has('request_apply'),
  };
}

export function isTerminalDoctorOperation(operation: DoctorRemediationOperation): boolean {
  return operation.phase !== 'running';
}

export function readActiveDoctorOperation(
  storage: Pick<Storage, 'getItem'> | undefined = browserStorage(),
): ActiveDoctorOperation | null {
  if (!storage) return null;
  try {
    const value = storage.getItem(ACTIVE_OPERATION_KEY);
    if (!value) return null;
    const parsed = ActiveDoctorOperationSchema.safeParse(JSON.parse(value));
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

export function saveActiveDoctorOperation(
  operation: ActiveDoctorOperation,
  storage: Pick<Storage, 'setItem'> | undefined = browserStorage(),
): void {
  if (!storage) return;
  try {
    storage.setItem(ACTIVE_OPERATION_KEY, JSON.stringify(operation));
  } catch {
    // Status resume is best-effort presentation state. The operation owner
    // remains authoritative even when browser storage is unavailable.
  }
}

export function sameDoctorScope(left: WireScope, right: WireScope): boolean {
  return (
    left.project_id === right.project_id &&
    left.storage_mode === right.storage_mode &&
    left.store_root === right.store_root
  );
}

export function sameDoctorAuthorityScope(
  left: DoctorRemediationAuthorityScope,
  right: DoctorRemediationAuthorityScope,
): boolean {
  if (left.scope_kind !== right.scope_kind) return false;
  if (left.scope_kind === 'profile' && right.scope_kind === 'profile') {
    return (
      left.brain_id === right.brain_id &&
      left.profile_id === right.profile_id &&
      left.profile_sessions_binding_digest === right.profile_sessions_binding_digest &&
      left.scope_digest === right.scope_digest
    );
  }
  if (left.scope_kind === 'project' && right.scope_kind === 'project') {
    return (
      left.project_id === right.project_id &&
      left.repository_id === right.repository_id &&
      left.worktree_id === right.worktree_id &&
      left.reference === right.reference &&
      left.scope_digest === right.scope_digest
    );
  }
  return false;
}

function browserStorage(): Storage | undefined {
  return typeof window === 'undefined' ? undefined : window.localStorage;
}
