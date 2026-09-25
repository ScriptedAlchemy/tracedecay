import { useId, useState } from 'react';
import * as Dialog from '@radix-ui/react-dialog';
import { X } from 'lucide-react';
import type { WorkflowDefinition } from '../../contracts/index.ts';
import { scopeWritable, useScope } from '../../data/scope/store.ts';
import { cn } from '../../ui/cn.ts';
import { GradeTag } from '../../ui/EvidenceGrade.tsx';
import { formatMicrosUtcClock } from '../../ui/format.ts';
import { Lamp, Panel } from '../../ui/instrument.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import {
  definitionKey,
  lifecycleStateTone,
  lifecycleTarget,
  type LifecycleReceipt,
  type ReceiptLedger,
  type WorkflowLifecycleAction,
} from './workflowLedger.ts';
import { useWorkflowLifecycle } from './workflowQueries.ts';
import {
  WORKFLOW_ACTIVATE_DEFINITION_ROUTE,
  WORKFLOW_REJECT_DEFINITION_ROUTE,
  WORKFLOW_RETIRE_DEFINITION_ROUTE,
} from './workflowRoutes.ts';

/**
 * Daemon-validated compare-and-swap. Three commands, one expected revision,
 * one explicit confirmation surface that states exactly what will be sent and
 * what the daemon, not this page, decides. The result region is populated
 * only by the daemon's receipt or its typed refusal; nothing here turns green
 * before the answer arrives.
 */

const INPUT_CLASS =
  'min-h-[var(--touch-target-min)] rounded-panel border border-edge-subtle bg-surface-1 px-2 font-mono text-sm text-text-primary focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent';

const COMMAND_CLASS =
  'flex min-h-[var(--touch-target-min)] w-full min-w-0 items-center gap-2.5 border border-edge-subtle px-2.5 py-1.5 text-left hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-60';

const ACTIONS: readonly {
  action: WorkflowLifecycleAction;
  label: string;
  from: string;
  operation: string;
}[] = [
  {
    action: 'activate',
    label: 'Activate',
    from: 'candidate or validated',
    operation: WORKFLOW_ACTIVATE_DEFINITION_ROUTE.operation,
  },
  {
    action: 'retire',
    label: 'Retire',
    from: 'active',
    operation: WORKFLOW_RETIRE_DEFINITION_ROUTE.operation,
  },
  {
    action: 'reject',
    label: 'Reject',
    from: 'candidate or validated',
    operation: WORKFLOW_REJECT_DEFINITION_ROUTE.operation,
  },
];

export function LifecyclePanel({
  definition,
  receipts,
  onReceipt,
}: {
  definition: WorkflowDefinition;
  receipts: ReceiptLedger;
  onReceipt: (receipt: LifecycleReceipt) => void;
}) {
  const scope = useScope((state) => state.scope);
  const writability = scopeWritable(scope);
  const lifecycle = useWorkflowLifecycle();
  const [revision, setRevision] = useState('1');
  const [staged, setStaged] = useState<WorkflowLifecycleAction | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const revisionId = useId();
  const parsedRevision = Number.parseInt(revision, 10);
  const validRevision = Number.isInteger(parsedRevision) && parsedRevision >= 1;
  const sessionReceipt =
    receipts.get(definitionKey(definition.definition_id, definition.definition_version)) ?? null;
  const lastSent = lifecycle.variables ?? null;

  const send = (action: WorkflowLifecycleAction) => {
    if (!validRevision) return;
    setStaged(null);
    setConfirmed(false);
    lifecycle.mutate(
      {
        action,
        definitionId: definition.definition_id,
        definitionVersion: definition.definition_version,
        expectedRevision: parsedRevision,
      },
      {
        onSuccess: (result, command) => {
          if (result.outcome === 'value') {
            onReceipt({
              action: command.action,
              expectedRevision: command.expectedRevision,
              disposition: result.value,
              answeredAtMillis: Date.now(),
            });
          }
        },
      },
    );
  };

  return (
    <Panel
      legend="Lifecycle · daemon-validated CAS"
      bodyClassName="flex min-w-0 flex-col gap-3 p-2.5"
      actions={
        <span className="td-legend shrink-0 text-text-muted" data-testid="workflow-scope-writability">
          scope {writability.state.replace('_', ' ')}
        </span>
      }
    >
      <div className="flex min-w-0 flex-col gap-1">
        <label className="flex min-w-0 flex-col gap-0.5 text-sm text-text-muted" htmlFor={revisionId}>
          <span className="td-legend">Expected revision · CAS input</span>
          <input
            id={revisionId}
            value={revision}
            inputMode="numeric"
            onChange={(event) => setRevision(event.target.value)}
            aria-invalid={!validRevision}
            className={cn(INPUT_CLASS, 'w-28')}
          />
        </label>
        <p className="text-sm leading-snug text-text-muted">
          The current revision is not served by any read route. A registered candidate starts at 1
          and every applied transition increments it; the daemon compares this value and answers a
          typed conflict when it is stale.{' '}
          {sessionReceipt === null
            ? ''
            : `The last answer this session for v${definition.definition_version} reported revision ${sessionReceipt.disposition.revision}.`}
        </p>
        {validRevision ? null : (
          <p role="alert" className="text-sm text-state-error">
            Expected revision must be a whole number of at least 1; nothing is sent until it is.
          </p>
        )}
      </div>

      <ul className="flex min-w-0 flex-col gap-1.5" aria-label="Lifecycle commands">
        {ACTIONS.map((entry) => (
          <li key={entry.action}>
            <button
              type="button"
              disabled={!validRevision || lifecycle.isPending}
              onClick={() => {
                setStaged(entry.action);
                setConfirmed(false);
              }}
              className={COMMAND_CLASS}
              data-lifecycle-action={entry.action}
            >
              <Lamp tone={lifecycleStateTone(lifecycleTarget(entry.action))} />
              <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                <span className="flex min-w-0 flex-wrap items-baseline gap-x-2">
                  <span className="td-value text-sm text-text-primary">
                    {entry.label} v{definition.definition_version}
                  </span>
                  <span className="td-value text-xs text-text-muted" data-cell="numeric">
                    expected_rev = {validRevision ? parsedRevision : '?'}
                  </span>
                </span>
                <span className="min-w-0 text-sm text-text-muted">
                  {entry.from} → {lifecycleTarget(entry.action)} · confirm before send
                </span>
              </span>
            </button>
          </li>
        ))}
      </ul>

      <LifecycleResult
        pending={lifecycle.isPending}
        result={lifecycle.data}
        lastSent={lastSent}
        receipt={sessionReceipt}
      />

      <ConfirmDialog
        action={staged}
        definition={definition}
        expectedRevision={parsedRevision}
        writability={writability}
        confirmed={confirmed}
        onConfirmedChange={setConfirmed}
        onDismiss={() => {
          setStaged(null);
          setConfirmed(false);
        }}
        onSend={send}
      />
    </Panel>
  );
}

function LifecycleResult({
  pending,
  result,
  lastSent,
  receipt,
}: {
  pending: boolean;
  result: ReturnType<typeof useWorkflowLifecycle>['data'];
  lastSent: { action: WorkflowLifecycleAction; expectedRevision: number } | null;
  receipt: LifecycleReceipt | null;
}) {
  return (
    <div
      className="flex min-w-0 flex-col gap-1.5 border-t border-edge-subtle pt-2"
      data-testid="workflow-lifecycle-result"
      aria-live="polite"
    >
      <span className="td-legend">last lifecycle result</span>
      {pending && lastSent !== null ? (
        <StateChip
          kind="loading"
          detail={`sending ${lastSent.action} · expected revision ${lastSent.expectedRevision} · awaiting the daemon's compare-and-swap`}
        />
      ) : result === undefined ? (
        <p className="text-sm text-text-muted">
          No lifecycle command has been sent for this version in this session. Nothing here is a
          success until the daemon answers one.
        </p>
      ) : result.outcome === 'refused' ? (
        <div className="flex min-w-0 flex-col gap-1">
          <StateChip kind={result.state} detail={result.detail} />
          <p className="text-sm text-text-muted">
            {lastSent === null ? 'The command' : `${lastSent.action} · expected revision ${lastSent.expectedRevision}`}{' '}
            did not transition anything.
            {result.state === 'conflicting'
              ? ' The stored revision is not the one named; re-read before retrying with the revision the daemon reports.'
              : ''}
          </p>
        </div>
      ) : (
        <div className="flex min-w-0 flex-col gap-1" data-lifecycle-receipt={result.value.state}>
          <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-sm">
            <Lamp tone={lifecycleStateTone(result.value.state)} />
            <span className="uppercase tracking-[0.1em] text-text-secondary text-3xs">
              disposition {result.value.state}
            </span>
            <span className="td-value text-text-secondary" data-cell="numeric">
              revision {result.value.revision}
            </span>
            <GradeTag grade="EXACT" source="cas receipt" />
          </div>
          <p className="td-value text-xs text-text-muted">
            {result.value.definition_id} v{result.value.definition_version} · transitioned{' '}
            {formatMicrosUtcClock(result.value.transitioned_at)} UTC
            {receipt === null
              ? ''
              : ` · answered ${new Date(receipt.answeredAtMillis).toISOString().slice(11, 19)} UTC`}
          </p>
        </div>
      )}
    </div>
  );
}

function ConfirmDialog({
  action,
  definition,
  expectedRevision,
  writability,
  confirmed,
  onConfirmedChange,
  onDismiss,
  onSend,
}: {
  action: WorkflowLifecycleAction | null;
  definition: WorkflowDefinition;
  expectedRevision: number;
  writability: ReturnType<typeof scopeWritable>;
  confirmed: boolean;
  onConfirmedChange: (confirmed: boolean) => void;
  onDismiss: () => void;
  onSend: (action: WorkflowLifecycleAction) => void;
}) {
  const entry = ACTIONS.find((candidate) => candidate.action === action) ?? null;
  const sendable = writability.state === 'writable';
  return (
    <Dialog.Root open={entry !== null} onOpenChange={(open) => (open ? undefined : onDismiss())}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/60" />
        <Dialog.Content className="fixed left-1/2 top-1/2 z-50 max-h-[calc(100dvh-2rem)] w-[min(34rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 overflow-y-auto border border-edge-strong bg-surface-1 p-4 shadow-xl">
          {entry === null ? null : (
            <div className="flex flex-col gap-3">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <Dialog.Title className="td-title text-text-primary">
                    Confirm {entry.label.toLowerCase()} · {definition.definition_id} v
                    {definition.definition_version}
                  </Dialog.Title>
                  <Dialog.Description className="mt-1 text-sm text-text-muted">
                    One compare-and-swap command. The daemon validates, checks permission, compares
                    the expected revision, and answers with the stored disposition or a typed
                    refusal. This dialog changes nothing by itself.
                  </Dialog.Description>
                </div>
                <Dialog.Close aria-label="Close lifecycle confirmation" className="td-hit group -mr-2 -mt-2 shrink-0">
                  <span className="inline-flex size-6 items-center justify-center text-text-muted group-hover:bg-surface-2 group-hover:text-text-primary">
                    <X aria-hidden size={16} />
                  </span>
                </Dialog.Close>
              </div>

              <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 border border-edge-subtle bg-surface-0 p-2.5 text-sm">
                <dt className="td-legend">operation</dt>
                <dd className="td-value break-all text-text-secondary">{entry.operation}</dd>
                <dt className="td-legend">definition</dt>
                <dd className="td-value break-all text-text-secondary">{definition.definition_id}</dd>
                <dt className="td-legend">version</dt>
                <dd className="td-value text-text-secondary">{definition.definition_version}</dd>
                <dt className="td-legend">expected revision</dt>
                <dd className="td-value text-text-secondary">{expectedRevision}</dd>
                <dt className="td-legend">transition</dt>
                <dd className="text-text-secondary">
                  {entry.from} → {lifecycleTarget(entry.action)}
                </dd>
                <dt className="td-legend">validation</dt>
                <dd className="text-text-secondary">
                  daemon-side on dispatch
                  {entry.action === 'activate'
                    ? '; structural revalidation and tool-catalog admission of every step operation gate activation'
                    : ''}
                </dd>
                <dt className="td-legend">permission</dt>
                <dd className="text-text-secondary">
                  decided by the daemon for this actor; a refusal is shown verbatim in the result
                </dd>
                <dt className="td-legend">scope</dt>
                <dd
                  className={cn(
                    'text-text-secondary',
                    writability.state !== 'writable' && 'text-state-locked',
                  )}
                  data-testid="workflow-confirm-scope"
                >
                  {writability.state === 'writable'
                    ? `writable · lands on ${writability.target}`
                    : writability.reason}
                </dd>
              </dl>

              <label
                className={cn(
                  'flex items-center gap-1 border border-edge-subtle py-1 pr-3 text-xs text-text-secondary',
                  sendable ? 'cursor-pointer' : 'opacity-60',
                )}
              >
                <input
                  type="checkbox"
                  className="td-check"
                  checked={confirmed}
                  disabled={!sendable}
                  onChange={(event) => onConfirmedChange(event.target.checked)}
                />
                <span className="min-w-0">
                  I confirm this compare-and-swap against expected revision {expectedRevision}.
                </span>
              </label>

              <div className="flex flex-wrap justify-end gap-2">
                <Dialog.Close className="min-h-[var(--touch-target-min)] border border-edge-subtle px-3 text-body text-text-secondary hover:bg-surface-3">
                  Cancel
                </Dialog.Close>
                <button
                  type="button"
                  disabled={!sendable || !confirmed}
                  onClick={() => onSend(entry.action)}
                  className="min-h-[var(--touch-target-min)] border border-edge-strong px-3 text-body text-text-primary hover:bg-surface-3 disabled:cursor-not-allowed disabled:opacity-60"
                >
                  Send {entry.label.toLowerCase()}
                </button>
              </div>
            </div>
          )}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
