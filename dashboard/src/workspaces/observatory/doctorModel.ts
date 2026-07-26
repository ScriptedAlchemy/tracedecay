import type {
  DashboardDoctorRemediationDescriptorV1,
  DoctorEvidenceState,
  DoctorFindingFamily,
  DoctorRemediationOperation,
  DoctorReportEntry,
  WireScope,
  WireLegalActionRef,
} from '../../contracts/wire.ts';
import { ScopeSchema } from '../../contracts/wire.ts';
import { z } from 'zod';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

const ACTIVE_OPERATION_KEY = 'tracedecay.doctor.active-operation.v3';
const ActiveDoctorOperationSchema = z.object({
  schema_revision: z.literal(3),
  operation_id: z.string(),
  transport_scope: ScopeSchema,
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

/** How one Doctor evidence state is presented, in every place it is presented.
 *
 * This used to be two tables. `doctorModel.ts` carried `label`/`tokenClass`/
 * `domainState` for the Doctor inspector and `storageModel.ts` carried
 * `label`/`tokenClass`/`dotClass` for the storage cards, both exported as
 * `doctorEvidencePresentation` from sibling files in this directory — so which
 * fields a caller got depended on which of the two it happened to import, and
 * the shared `label`/`tokenClass` columns were maintained twice by hand. */
export interface DoctorEvidencePresentation {
  label: string;
  /** Foreground token for chip text. */
  tokenClass: string;
  /** The same token as a background, for the chip's dot. Derived rather than
   * listed: one state has one colour, and a dot that disagrees with its label
   * is the only bug this field could have. */
  dotClass: string;
  /** The `StateChip` kind this evidence state maps to. */
  domainState: DomainStateKind;
}

const EVIDENCE_TOKENS: Record<
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

export function doctorEvidencePresentation(
  state: DoctorEvidenceState,
): DoctorEvidencePresentation {
  const tokens = EVIDENCE_TOKENS[state];
  return { ...tokens, dotClass: tokens.tokenClass.replace('text-', 'bg-') };
}

export function remediationForEntry(
  entry: DoctorReportEntry,
  descriptors: DashboardDoctorRemediationDescriptorV1[],
): DashboardDoctorRemediationDescriptorV1 | undefined {
  const operation = entry.finding.remediation?.owning_operation;
  return operation
    ? descriptors.find((descriptor) => descriptor.operation === operation)
    : undefined;
}

export function availableRemediationActions(
  descriptor: DashboardDoctorRemediationDescriptorV1,
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

function browserStorage(): Storage | undefined {
  return typeof window === 'undefined' ? undefined : window.localStorage;
}
