import * as Dialog from '@radix-ui/react-dialog';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type { LucideIcon } from 'lucide-react';
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  CircleSlash,
  Clock,
  FileSearch,
  HelpCircle,
  ShieldCheck,
  ShieldX,
  X,
  XCircle,
} from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import {
  assertNever,
  type DashboardDoctorRemediationDescriptorV1,
  type DoctorEvidenceStateV1,
  type DoctorFindingsPayloadV1,
  type DoctorOwningSurfaceV1,
  type DoctorRemediationApplyRequestV1,
  type DoctorRemediationOperationV1,
  type DoctorRemediationPayloadV1,
  type DoctorRemediationPreviewRequestV1,
  type DoctorRemediationTargetV1,
  type DoctorReportEntryV1,
  type DoctorReportCoverageV1,
  type ResolvedScope,
  type DashboardLegalActionRefV1,
} from '../../contracts/generated.ts';
import {
  applyDoctorRemediation,
  doctorFindingsQueryKey,
  doctorOperationQueryKey,
  fetchDoctorFindings,
  fetchDoctorRemediationStatus,
  previewDoctorRemediation,
  type DoctorWriteResult,
} from '../../data/query/doctor.ts';
import { mintBrowserIdempotencyKey } from '../../data/identity.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import {
  scopeWritable,
  scopeWriteSentence,
  useScope,
  type ScopeWritability,
} from '../../data/scope/store.ts';
import { EnvelopeTruth } from '../../ui/EnvelopeTruth.tsx';
import { ReadModelState, envelopeReadState } from '../../ui/ReadSection.tsx';
import { EvidenceTruthStrip } from '../../ui/EvidenceTruthStrip.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { cn } from '../../ui/cn.ts';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid.tsx';
import {
  availableRemediationActions,
  doctorEvidencePresentation,
  doctorFamilyLabel,
  doctorOwningSurfaceLabel,
  isTerminalDoctorOperation,
  readActiveDoctorOperation,
  remediationForEntry,
  saveActiveDoctorOperation,
  sameDoctorScope,
  type RemediationActionAvailability,
} from './doctorModel.ts';

type SelectedRemediation = {
  entry: DoctorReportEntryV1;
  descriptor: DashboardDoctorRemediationDescriptorV1;
  target: DoctorRemediationTargetV1;
  actions: RemediationActionAvailability;
  idempotencyKey: string;
};

/** A finding the canonical report attached no remediation reference to. */
const NO_REMEDIATION_ACTIONS: RemediationActionAvailability = {
  canPreview: false,
  canApply: false,
  dispatchable: false,
};

/** Canonical Doctor finding inspector and owner-operation handoff. The component
 * renders server-supplied findings/actions only; it owns no diagnosis or repair. */
export function DoctorInspector() {
  const queryClient = useQueryClient();
  // The panel reads the scope the rest of the workspace reads, and the rail's
  // health dot reads the same one — so the diagnosis on screen belongs to the
  // project named in the scope bar rather than to whichever project the daemon
  // happens to have active.
  const scope = useScope((s) => s.scope);
  const writability = scopeWritable(scope);
  // Stable per scope: it is an effect dependency and the invalidation target.
  const findingsKey = useMemo(() => doctorFindingsQueryKey(scope), [scope]);
  const findings = useQuery({
    queryKey: findingsKey,
    queryFn: () => fetchDoctorFindings(scope),
    refetchInterval: 30_000,
  });
  const [selected, setSelected] = useState<SelectedRemediation | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [activeOperation, setActiveOperation] = useState(readActiveDoctorOperation);
  const reobservedOperation = useRef<string | null>(null);
  const currentScope =
    findings.data?.outcome === 'envelope' ? findings.data.envelope.scope : null;
  const activeTransportScopeMatches =
    activeOperation && currentScope
      ? sameDoctorScope(activeOperation.transport_scope, currentScope)
      : false;

  const rememberOperation = (result: DoctorWriteResult) => {
    const operation = operationFromResult(result);
    if (!operation || result.outcome !== 'envelope') return;
    const active = {
      schema_revision: 3 as const,
      operation_id: operation.operation_id,
      transport_scope: result.envelope.scope,
    };
    setActiveOperation(active);
    saveActiveDoctorOperation(active);
  };

  const preview = useMutation({
    mutationFn: (request: DoctorRemediationPreviewRequestV1) =>
      previewDoctorRemediation(scope, request),
    onSuccess: rememberOperation,
  });
  const apply = useMutation({
    mutationFn: (request: DoctorRemediationApplyRequestV1) => applyDoctorRemediation(scope, request),
    onSuccess: (result) => {
      rememberOperation(result);
      setConfirmed(false);
    },
  });
  const status = useQuery({
    queryKey: doctorOperationQueryKey(scope, activeOperation?.operation_id ?? 'inactive'),
    queryFn: () => fetchDoctorRemediationStatus(scope, activeOperation!.operation_id),
    enabled: activeOperation != null && activeTransportScopeMatches,
    refetchInterval: (query) => {
      const operation = operationFromResult(query.state.data);
      return operation?.phase === 'running' ? 2_000 : false;
    },
  });

  const statusOperation = operationFromResult(status.data);
  const observedOperation =
    statusOperation ?? operationFromResult(apply.data) ?? operationFromResult(preview.data);

  useEffect(() => {
    if (
      observedOperation &&
      isTerminalDoctorOperation(observedOperation) &&
      observedOperation.phase !== 'previewed' &&
      reobservedOperation.current !== observedOperation.operation_id
    ) {
      reobservedOperation.current = observedOperation.operation_id;
      void queryClient.invalidateQueries({ queryKey: findingsKey });
    }
  }, [observedOperation, queryClient, findingsKey]);

  const selectedPreviewId = useMemo(() => {
    if (!selected) return null;
    for (const result of [status.data, preview.data]) {
      const operation = operationFromResult(result);
      if (
        operation?.owning_operation === selected.descriptor.operation &&
        operation.preview_id
      ) {
        return operation.preview_id;
      }
    }
    return null;
  }, [preview.data, selected, status.data]);

  const selectedAuthorityScope = useMemo(() => {
    if (!selected) return null;
    for (const result of [status.data, apply.data]) {
      const operation = operationFromResult(result);
      if (operation?.owning_operation === selected.descriptor.operation) {
        return operation.effect_receipt?.scope ?? null;
      }
    }
    return null;
  }, [apply.data, selected, status.data]);

  const submitApply = () => {
    if (!selected) return;
    apply.mutate({
      operation: selected.descriptor.operation,
      target: selected.target,
      preview_id: selectedPreviewId,
      idempotency_key: selected.idempotencyKey,
      confirmed:
        selected.descriptor.action_confirmation === 'required' ? confirmed : true,
    });
  };

  return (
    <section className="border-b border-edge-subtle" aria-label="Doctor diagnosis">
      <div className="flex flex-wrap items-center gap-2 px-4 pt-4">
        <Activity aria-hidden size={14} className="text-accent" />
        <h2 className="text-sm font-semibold tracking-tight">Doctor diagnosis</h2>
        <span className="text-2xs text-text-muted">
          canonical evidence and owner-supplied remediation
        </span>
      </div>

      <DoctorFindings
        result={findings.data}
        pending={findings.isPending}
        refreshing={findings.isFetching}
        onRefresh={() => void findings.refetch()}
        onInspect={(entry, descriptor, legalActions) => {
          const target = descriptor.target;
          if (!target) return;
          setConfirmed(false);
          setSelected({
            entry,
            descriptor,
            target,
            actions: availableRemediationActions(descriptor, legalActions),
            idempotencyKey: mintBrowserIdempotencyKey('dashboard-doctor'),
          });
        }}
        onPreview={(operation) => preview.mutate(operation)}
        previewing={preview.isPending}
        writability={writability}
      />

      <OperationStatus
        result={status.data ?? apply.data ?? preview.data}
        pending={status.isFetching || apply.isPending || preview.isPending}
        operationId={activeOperation?.operation_id ?? null}
        transportScopeMismatch={
          activeOperation != null && currentScope != null && !activeTransportScopeMatches
        }
      />

      <RemediationDialog
        selection={selected}
        confirmed={confirmed}
        previewId={selectedPreviewId}
        authorityScope={selectedAuthorityScope}
        applying={apply.isPending}
        result={apply.data}
        writability={writability}
        onConfirmedChange={setConfirmed}
        onOpenChange={(open) => {
          if (!open) {
            setSelected(null);
            setConfirmed(false);
            apply.reset();
          }
        }}
        onApply={submitApply}
      />
    </section>
  );
}

function DoctorFindings({
  result,
  pending,
  refreshing,
  previewing,
  writability,
  onRefresh,
  onInspect,
  onPreview,
}: {
  result: EnvelopeResult<DoctorFindingsPayloadV1> | undefined;
  pending: boolean;
  refreshing: boolean;
  previewing: boolean;
  writability: ScopeWritability;
  onRefresh: () => void;
  onInspect: (
    entry: DoctorReportEntryV1,
    descriptor: DashboardDoctorRemediationDescriptorV1,
    legalActions: DashboardLegalActionRefV1[],
  ) => void;
  onPreview: (request: { operation: string; target: DoctorRemediationTargetV1 }) => void;
}) {
  const read = envelopeReadState(pending, result, {
    loading: 'requesting canonical Doctor findings',
    unknown: 'no Doctor response recorded',
  });
  if (read.kind === 'blocked') {
    return <ReadModelState kind={read.state} detail={read.detail} />;
  }

  const envelope = read.value;
  if (envelope.payload.entries.length === 0) {
    return (
      <>
        <EnvelopeTruth envelope={envelope} refreshing={refreshing} onRefresh={onRefresh} />
        <DoctorReportCoverageGaps coverage={envelope.payload.report_coverage} />
        <ReadModelState kind={envelope.domain_state} detail={envelope.payload.note} />
      </>
    );
  }

  return (
    <>
      <EnvelopeTruth envelope={envelope} refreshing={refreshing} onRefresh={onRefresh} />
      <DoctorReportCoverageGaps coverage={envelope.payload.report_coverage} />
      <OverviewGrid>
        {envelope.payload.entries.map((entry, index) => {
          const descriptor = remediationForEntry(entry, envelope.payload.remediations);
          const target = descriptor?.target ?? null;
          const actions = descriptor
            ? availableRemediationActions(descriptor, envelope.legal_actions)
            : NO_REMEDIATION_ACTIONS;
          return (
            <FindingCard
              key={`${entry.finding.family}:${entry.storage_kind ?? 'general'}:${index}`}
              entry={entry}
              descriptor={descriptor}
              target={target}
              actions={actions}
              previewing={previewing}
              writability={writability}
              onPreview={onPreview}
              onInspect={() => {
                if (descriptor) onInspect(entry, descriptor, envelope.legal_actions);
              }}
            />
          );
        })}
      </OverviewGrid>
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
    coverage?.families.filter(
      (family) => family.consultation.status === 'unavailable',
    ) ?? [];
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

/**
 * What the current scope means for the controls beside it.
 *
 * Rendered in every state including `writable`, because the aggregate scope is
 * writable and a remediation issued under it lands on one project — the reader
 * is told which rather than being left to assume it applies across the
 * registry. Exhaustive, so a new writability state has to choose its wording
 * here instead of silently rendering an unexplained disabled control.
 */
function ScopeWriteNote({ writability }: { writability: ScopeWritability }) {
  return (
    <p
      data-scope-writability={writability.state}
      className="mt-2 text-2xs text-text-secondary"
    >
      {scopeWriteSentence(writability, {
        writable: (target) => `Remediations apply to ${target}.`,
      })}
    </p>
  );
}

function FindingCard({
  entry,
  descriptor,
  target,
  actions,
  previewing,
  writability,
  onPreview,
  onInspect,
}: {
  entry: DoctorReportEntryV1;
  descriptor: DashboardDoctorRemediationDescriptorV1 | undefined;
  target: DoctorRemediationTargetV1 | null;
  actions: RemediationActionAvailability;
  previewing: boolean;
  writability: ScopeWritability;
  onPreview: (request: { operation: string; target: DoctorRemediationTargetV1 }) => void;
  onInspect: () => void;
}) {
  const { finding } = entry;
  const authorized = actions.canPreview || actions.canApply;
  const scopeBlocked = writability.state !== 'writable';
  return (
    <OverviewCard title={doctorFamilyLabel(finding.family)}>
      <div className="flex flex-col gap-2">
        <EvidenceBadge state={finding.state} />
        <EvidenceTruthStrip
          coverage={{ completeness: finding.coverage.completeness }}
          citations={finding.evidence.length}
        />
        <p className="text-xs text-text-secondary">{finding.coverage.statement}</p>
        {descriptor ? (
          <div className="rounded-[var(--radius-chip)] bg-surface-2 p-2.5">
            <p className="text-xs text-text-secondary">{descriptor.summary}</p>
            <p className="mt-1 truncate font-mono text-2xs text-text-muted">
              {descriptor.operation}
            </p>
            {actions.dispatchable && target ? (
              <>
                <div className="mt-2 flex flex-wrap gap-2">
                  {actions.canPreview ? (
                    <button
                      type="button"
                      className={secondaryButtonClass}
                      onClick={() => onPreview({ operation: descriptor.operation, target })}
                      disabled={previewing || scopeBlocked}
                    >
                      <span className={secondaryBezelClass}>
                        {previewing ? 'Previewing' : 'Preview'}
                      </span>
                    </button>
                  ) : null}
                  {actions.canApply ? (
                    <button
                      type="button"
                      className={primaryButtonClass}
                      onClick={onInspect}
                      disabled={scopeBlocked}
                    >
                      <span className={primaryBezelClass}>Review remediation</span>
                    </button>
                  ) : null}
                </div>
                {/* A preview is a POST, so the gateway treats it as a write and
                  * so does this card: both controls are gated on one reading,
                  * and the reading is stated rather than left to a greyed-out
                  * button to imply. */}
                <ScopeWriteNote writability={writability} />
              </>
            ) : null}
            <RemediationAvailabilityNote
              authorized={authorized}
              dispatchable={actions.dispatchable}
              surface={descriptor.surface}
            />
          </div>
        ) : null}
      </div>
    </OverviewCard>
  );
}

/** The lucide glyph for each evidence state, matching what `StateChip` draws
 * for the domain state that evidence state maps to — so a Doctor finding and
 * the chip beside it never disagree about what `stale` looks like. */
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

/**
 * A finding's evidence state, drawn the way `StateChip` draws a domain state:
 * the hue rides a lamp bar and an icon, and the label text sits on an
 * AA-contrast token.
 *
 * The label must not be painted in `evidence.tokenClass`: those are indicator
 * hues, and as 11px text on `--surface-2` five of them miss the 4.5:1 WCAG AA
 * threshold that applies at this size. `text-text-secondary` measures 8.64:1
 * there. The hue is not lost — it moves to the lamp and the glyph, where it is
 * decoration beside a text label rather than the only carrier of meaning, which
 * is the rule the whole state taxonomy is built on. `axe-observatory.ts` scans
 * these badges against a populated Doctor fixture.
 */
function EvidenceBadge({ state }: { state: DoctorEvidenceStateV1 }) {
  const evidence = doctorEvidencePresentation(state);
  const Icon = EVIDENCE_ICON[state];
  return (
    <span
      className="relative inline-flex w-fit items-center gap-1.5 border border-edge-subtle bg-surface-2 py-[3px] pl-2.5 pr-2 text-2xs font-medium"
      data-evidence-state={state}
    >
      <span aria-hidden className={cn('absolute inset-y-0 left-0 w-[2px]', evidence.dotClass)} />
      <Icon aria-hidden size={11} className={evidence.tokenClass} />
      <span className="text-text-secondary">{evidence.label}</span>
    </span>
  );
}

/** Why a finding shows no remediation button, told apart from each other.
 *
 * An owner that authorizes an action it must be handed a change for is not the
 * same thing as an owner that authorizes nothing, and reporting the first as
 * the second would deny an available repair. */
function RemediationAvailabilityNote({
  authorized,
  dispatchable,
  surface,
}: {
  authorized: boolean;
  dispatchable: boolean;
  surface: DoctorOwningSurfaceV1;
}) {
  if (!authorized) {
    return (
      <p className="mt-2 text-2xs text-text-muted">
        No authorized remediation action is currently available.
      </p>
    );
  }
  if (dispatchable) return null;
  return (
    <p className="mt-2 text-2xs text-text-secondary">
      Authorized by {doctorOwningSurfaceLabel(surface)}, which also supplies the exact
      change to apply. Doctor reports the finding; run the remediation there.
    </p>
  );
}

function OperationStatus({
  result,
  pending,
  operationId,
  transportScopeMismatch,
}: {
  result: DoctorWriteResult | undefined;
  pending: boolean;
  operationId: string | null;
  transportScopeMismatch: boolean;
}) {
  if (!operationId && !result) return null;
  // A control that declined to dispatch produced no operation, so this reports
  // the absence of one rather than a phase. `locked` is the state for a surface
  // that will not accept a change, and the reason is the scope authority's own.
  if (result?.outcome === 'not_dispatched') {
    return <ReadModelState kind="locked" detail={result.writability.reason} />;
  }
  if (transportScopeMismatch) {
    return (
      <ReadModelState
        kind="conflicting"
        detail="saved remediation belongs to a different dashboard scope"
      />
    );
  }
  // `pending && !result`: a re-poll that already has a reading keeps showing it
  // rather than blanking back to a loading chip.
  const read = envelopeReadState(pending && !result, result, {
    loading: 'checking remediation operation',
    unknown: 'operation status is unavailable',
    transport: 'operation owner unreachable',
  });
  if (read.kind === 'blocked') {
    return <ReadModelState kind={read.state} detail={read.detail} />;
  }
  const { payload } = read.value;
  if (payload.status === 'unavailable') {
    return (
      <ReadModelState
        kind={read.value.domain_state}
        detail={`remediation ${payload.reason.replaceAll('_', ' ')}`}
      />
    );
  }
  return (
    <div className="mx-4 mb-4 flex flex-wrap items-center gap-3 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 px-3 py-2">
      <StateChip kind={operationPhaseState(payload.operation.phase)} />
      <span className="text-xs text-text-secondary">
        Remediation {payload.operation.phase.replaceAll('_', ' ')}
      </span>
      <span className="truncate font-mono text-2xs text-text-muted">
        {payload.operation.operation_id}
      </span>
      {payload.operation.effect_receipt ? (
        <AuthorityScope scope={payload.operation.effect_receipt.scope} compact />
      ) : null}
      {payload.operation.execution ? (
        <span className="ml-auto text-2xs text-text-muted">
          receipt {payload.operation.execution.termination.replaceAll('_', ' ')}
        </span>
      ) : null}
    </div>
  );
}

function RemediationDialog({
  selection,
  confirmed,
  previewId,
  authorityScope,
  applying,
  result,
  writability,
  onConfirmedChange,
  onOpenChange,
  onApply,
}: {
  selection: SelectedRemediation | null;
  confirmed: boolean;
  previewId: string | null;
  authorityScope: ResolvedScope | null;
  applying: boolean;
  result: DoctorWriteResult | undefined;
  writability: ScopeWritability;
  onConfirmedChange: (confirmed: boolean) => void;
  onOpenChange: (open: boolean) => void;
  onApply: () => void;
}) {
  const confirmationRequired =
    selection?.descriptor.action_confirmation === 'required';
  return (
    <Dialog.Root open={selection != null} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/60" />
        <Dialog.Content className="fixed left-1/2 top-1/2 z-50 max-h-[calc(100dvh-2rem)] w-[min(36rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 p-5 shadow-xl">
          <div className="flex items-start justify-between gap-3">
            <div>
              <Dialog.Title className="text-base font-semibold tracking-tight">
                Confirm owner remediation
              </Dialog.Title>
              <Dialog.Description className="mt-1 text-xs text-text-muted">
                Doctor supplies the reference; the owning application operation rechecks authority
                and performs any effect.
              </Dialog.Description>
            </div>
            <Dialog.Close
              className="rounded-[var(--radius-chip)] p-1 text-text-muted hover:bg-surface-2 hover:text-text-primary"
              aria-label="Close remediation"
            >
              <X aria-hidden size={16} />
            </Dialog.Close>
          </div>

          {selection ? (
            <div className="mt-4 flex flex-col gap-3">
              <div className="rounded-[var(--radius-standard)] bg-surface-2 p-3">
                <p className="text-sm text-text-secondary">{selection.descriptor.summary}</p>
                <p className="mt-2 break-all font-mono text-2xs text-text-muted">
                  {selection.descriptor.operation}
                </p>
                <p className="mt-1 text-2xs text-text-muted">
                  owner {selection.descriptor.surface.replaceAll('_', ' ')}
                </p>
                <p className="mt-1 break-all font-mono text-2xs text-text-muted">
                  idempotency {selection.idempotencyKey}
                </p>
                {previewId ? (
                  <p className="mt-1 break-all font-mono text-2xs text-text-muted">
                    preview {previewId}
                  </p>
                ) : null}
              </div>

              <FindingEvidence entry={selection.entry} />

              {authorityScope ? (
                <AuthorityScope scope={authorityScope} />
              ) : (
                <StateChip
                  kind="unknown"
                  detail="authority scope will be resolved and rechecked by the owner at dispatch"
                />
              )}

              {confirmationRequired ? (
                <label className="flex cursor-pointer items-center gap-1 rounded-[var(--radius-standard)] border border-edge-subtle py-2 pr-3 text-xs text-text-secondary">
                  <input
                    type="checkbox"
                    checked={confirmed}
                    onChange={(event) => onConfirmedChange(event.target.checked)}
                    className="td-check"
                  />
                  <span className="min-w-0">
                    {authorityScope
                      ? 'I confirm this exact owner operation for the authority scope shown above and the displayed evidence.'
                      : 'I confirm this exact owner operation and the displayed evidence; the owner must resolve and recheck authority before any effect.'}
                  </span>
                </label>
              ) : null}

              <RemediationResult result={result} />
              {/* The scope reads the same here as on the card this dialog was
                * opened from, and it gates the apply for the same reason. */}
              <ScopeWriteNote writability={writability} />

              <div className="flex justify-end gap-2">
                <Dialog.Close className={secondaryButtonClass}>
                  <span className={secondaryBezelClass}>Cancel</span>
                </Dialog.Close>
                <button
                  type="button"
                  className={primaryButtonClass}
                  onClick={onApply}
                  disabled={
                    applying ||
                    !selection.actions.canApply ||
                    writability.state !== 'writable' ||
                    (confirmationRequired && !confirmed)
                  }
                >
                  <span className={primaryBezelClass}>
                    <ShieldCheck aria-hidden size={13} />
                    {applying ? 'Applying' : 'Apply remediation'}
                  </span>
                </button>
              </div>
            </div>
          ) : null}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function AuthorityScope({
  scope,
  compact = false,
}: {
  scope: ResolvedScope;
  compact?: boolean;
}) {
  const label = `project ${scope.project_id}`;
  if (compact) {
    return (
      <span
        className="truncate font-mono text-2xs text-text-muted"
        title={`${label} · ${scope.scope_digest}`}
      >
        authority {label}
      </span>
    );
  }
  const details = [
    ['Project', scope.project_id],
    ['Repository', scope.repository_id],
    ['Worktree', scope.worktree_id],
    ['Reference', scope.reference ?? 'none'],
    ['Scope digest', scope.scope_digest],
  ];
  return (
    <div
      className="rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 p-3"
      aria-label="Remediation authority scope"
    >
      <p className="text-2xs font-medium uppercase tracking-wide text-text-muted">
        project authority
      </p>
      <dl className="mt-2 grid gap-1">
        {details.map(([term, value]) => (
          <div key={term} className="grid gap-1 text-2xs sm:grid-cols-[9rem_1fr]">
            <dt className="text-text-muted">{term}</dt>
            <dd className="break-all font-mono text-text-secondary">{value}</dd>
          </div>
        ))}
      </dl>
    </div>
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

function RemediationResult({ result }: { result: DoctorWriteResult | undefined }) {
  if (!result) return null;
  // Nothing was sent, so there is no owner result to report. The adjacent
  // `ScopeWriteNote` already carries the reason, so this stays silent rather
  // than printing it twice.
  if (result.outcome === 'not_dispatched') return null;
  if (result.outcome === 'transport') {
    return <StateChip kind={result.state} detail={result.detail ?? 'owner unreachable'} />;
  }
  const { payload } = result.envelope;
  return payload.status === 'unavailable' ? (
    <StateChip
      kind={result.envelope.domain_state}
      detail={payload.reason.replaceAll('_', ' ')}
    />
  ) : (
    <StateChip
      kind={operationPhaseState(payload.operation.phase)}
      detail={payload.operation.phase.replaceAll('_', ' ')}
    />
  );
}

/** Accepts a write result too, so a control that declined to dispatch reports
 * no operation rather than needing a separate path to say the same thing. */
function operationFromResult(
  result: DoctorWriteResult | undefined,
): DoctorRemediationOperationV1 | null {
  if (result?.outcome !== 'envelope') return null;
  const { payload } = result.envelope;
  return payload.status === 'operation' ? payload.operation : null;
}

function operationPhaseState(
  phase: DoctorRemediationOperationV1['phase'],
): DomainStateKind {
  switch (phase) {
    case 'previewed':
    case 'completed':
      return 'ready';
    case 'running':
    case 'partial':
      return 'partial';
    case 'cancelled':
      return 'cancelled';
    case 'timed_out':
      return 'timed_out';
    case 'failed':
    case 'effect_unknown':
      return 'error';
    default:
      return assertNever(phase);
  }
}

/* Operation controls, split into the box a pointer has to be able to hit and
 * the bezel a reader sees.
 *
 * These are 24.5px tall by design — they sit inside panel headers and evidence
 * strips, and a row of 44px slabs there would be a different console. So the
 * element carries the 44px minimum (`.td-hit`, see `tailwind.css`) and the
 * bezel goes on being drawn at `h-7` inside it. Hover moves to the group so
 * the whole hit area lights the bezel, not just the bezel itself. */
const secondaryButtonClass = 'td-hit group disabled:cursor-wait disabled:opacity-60';
const secondaryBezelClass =
  'inline-flex h-7 items-center justify-center gap-1.5 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 px-2.5 text-2xs font-medium text-text-secondary group-hover:text-text-primary';
const primaryButtonClass = 'td-hit group disabled:cursor-not-allowed disabled:opacity-50';
const primaryBezelClass =
  'inline-flex h-7 items-center justify-center gap-1.5 rounded-[var(--radius-standard)] border border-accent/50 bg-accent/15 px-2.5 text-2xs font-semibold text-text-primary group-hover:border-accent';
