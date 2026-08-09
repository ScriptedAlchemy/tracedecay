import { useState } from 'react';
import type {
  PrepareWorkProductMutationRequestV1,
  WorkGraphReadV1,
  WorkProductMutationReceiptV1,
  WorkProductMutationRequestV1,
  WorkProductSelectionScopeV1,
  WorkProjection,
  WorkProjectionSnapshotV1,
} from '../../contracts/index.ts';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import type { WorkResult } from './workApi.ts';
import { useWorkCommand, useWorkReadAction } from './workQueries.ts';
import {
  WORK_ACCEPT_TASK_ROUTE,
  WORK_MUTATE_GRAPH_ROUTE,
  WORK_PREPARE_GRAPH_MUTATION_ROUTE,
  WORK_REPLAN_DEPENDENCIES_ROUTE,
} from './workRoutes.ts';
import { useWorkGraphViews } from './workViewsQueries.ts';

/**
 * The controls for one task.
 *
 * The daemon is the only authority over whether a command is legal. This panel
 * submits generated requests and renders typed refusals rather than inferring
 * an allow-list from a projection.
 *
 * Legacy projection commands carry the projection's own `version` as
 * `expected_version`; product mutations receive their graph authority and
 * revision pins from the backend preparation response. Both paths are
 * compare-and-swap and surface a moved task as a typed conflict.
 */

/** A fresh idempotency key per attempt. Re-sending one command twice under the
 * same key is the daemon's cue that it is a retry, so a new key per attempt is
 * what keeps a genuine second command from being swallowed as one. */
function commandId(): string {
  return globalThis.crypto?.randomUUID?.() ?? `work-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function occurredAt(): number {
  return Date.now() * 1000;
}

/** What a finished command attempt reads as. `undefined` while nothing has been
 * sent, so a control that has never run says nothing rather than saying it
 * succeeded. */
function commandReading<T>(
  result: WorkResult<T> | undefined,
  pending: boolean,
  committed: (value: T) => string,
): { state: DomainStateKind; detail: string } | undefined {
  if (pending) return { state: 'loading', detail: 'sending' };
  if (result === undefined) return undefined;
  if (result.outcome === 'value') {
    return { state: 'ready', detail: committed(result.value) };
  }
  // The refusal's own taxonomy, untranslated: an unreachable daemon, a denied
  // write and a moved task are different facts, and mapping everything but
  // `conflicting` onto `error` dressed all of them in the same chip.
  return { state: result.state, detail: result.detail };
}

function CommandButton({
  label,
  disabled,
  onRun,
  reading,
}: {
  label: string;
  disabled: boolean;
  onRun: () => void;
  reading: ReturnType<typeof commandReading>;
}) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-2 border-b border-edge px-2 py-1.5 last:border-b-0">
      <button
        type="button"
        onClick={onRun}
        disabled={disabled}
        className="min-h-[44px] rounded-sm border border-edge px-2 py-1 text-2xs text-text-primary hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:text-text-muted"
      >
        {label}
      </button>
      {reading === undefined ? null : (
        <StateChip kind={reading.state} detail={reading.detail} />
      )}
    </div>
  );
}

export function WorkCommands({
  projection,
  snapshot,
}: {
  projection: WorkProjection;
  snapshot: WorkProjectionSnapshotV1;
}) {
  const acceptTask = useWorkCommand(WORK_ACCEPT_TASK_ROUTE);
  const admissionPreparation = useWorkReadAction(WORK_PREPARE_GRAPH_MUTATION_ROUTE);
  const admitExecution = useWorkCommand(WORK_MUTATE_GRAPH_ROUTE);
  const replan = useWorkCommand(WORK_REPLAN_DEPENDENCIES_ROUTE);
  const graph = useWorkGraphViews(true);
  const [dependencies, setDependencies] = useState<readonly string[]>(projection.dependencies);

  const selection = currentWorkSelection(graph.data);
  const candidates = snapshot.projections
    .map((candidate) => candidate.task_id)
    .filter((taskId) => taskId !== projection.task_id);
  const admissionReading =
    admitExecution.data !== undefined || admitExecution.isPending
      ? commandReading(
          admitExecution.data,
          admitExecution.isPending,
          (receipt) =>
            `${receipt.replayed ? 'replayed' : 'committed'} ${receipt.event.event_id} at graph version ${receipt.verified_graph_version.graph_version}, sequence ${receipt.verified_graph_version.event_sequence}`,
        )
      : mutationReading(
          admissionPreparation.data,
          admissionPreparation.isPending,
          'prepared exact execution admission',
        );

  async function prepareAndAdmitExecution(): Promise<void> {
    if (selection === undefined) return;
    const prepared = await admissionPreparation.mutateAsync({
      causation_event_id: null,
      evidence: [],
      selection,
      change: {
        change: 'admit_execution',
        task_id: projection.task_id,
      },
    });
    if (prepared.outcome === 'value' && prepared.value.mutation === 'admit_execution') {
      admitExecution.mutate(prepared.value);
    }
  }

  return (
    <Panel legend={`Commands · ${projection.title}`} bodyClassName="p-0">
      <dl className="sr-only">
        <dt>Task</dt>
        <dd>{projection.task_id}</dd>
        <dt>Version used for compare-and-swap</dt>
        <dd>{projection.version}</dd>
      </dl>

      <CommandButton
        label="Accept task"
        disabled={acceptTask.isPending}
        reading={commandReading(
          acceptTask.data,
          acceptTask.isPending,
          (value) => `committed at version ${value.version}`,
        )}
        onRun={() =>
          acceptTask.mutate({
            command_id: commandId(),
            expected_version: projection.version,
            occurred_at: occurredAt(),
            task_id: projection.task_id,
          })
        }
      />

      <CommandButton
        label="Admit execution"
        disabled={
          selection === undefined || admissionPreparation.isPending || admitExecution.isPending
        }
        reading={admissionReading}
        onRun={() => void prepareAndAdmitExecution()}
      />

      <div className="border-b border-edge px-2 py-1.5 last:border-b-0">
        <label
          htmlFor="work-replan-dependencies"
          className="block text-2xs text-text-secondary"
        >
          Dependencies
        </label>
        {/* The options are the other tasks this snapshot returned, so a
          * dependency can only ever name a task the daemon reported. A free
          * text field here would let the board send an identifier that does
          * not exist. */}
        <select
          id="work-replan-dependencies"
          multiple
          size={Math.min(Math.max(candidates.length, 2), 5)}
          value={[...dependencies]}
          onChange={(event) =>
            setDependencies(Array.from(event.target.selectedOptions, (option) => option.value))
          }
          className="mt-1 w-full rounded-sm border border-edge bg-surface-1 p-1 font-mono text-3xs text-text-primary focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          {candidates.map((taskId) => (
            <option key={taskId} value={taskId}>
              {taskId}
            </option>
          ))}
        </select>
        {candidates.length === 0 ? (
          <p className="mt-0.5 text-3xs text-text-muted">
            This snapshot returned no other task to depend on.
          </p>
        ) : null}
        <div className="mt-1">
          <CommandButton
            label="Replan dependencies"
            disabled={replan.isPending}
            reading={commandReading(
              replan.data,
              replan.isPending,
              (value) => `committed at version ${value.version}`,
            )}
            onRun={() =>
              replan.mutate({
                command_id: commandId(),
                dependencies: [...dependencies],
                expected_version: projection.version,
                occurred_at: occurredAt(),
                task_id: projection.task_id,
              })
            }
          />
        </div>
      </div>

    </Panel>
  );
}

type WorkCreateFields = {
  taskId: string;
  taskTitle: string;
  taskCreatedAt: string;
  taskUpdatedAt: string;
  effort: string;
  initiativeId: string;
  initiativeTitle: string;
  initiativeCreatedAt: string;
  planId: string;
  planTitle: string;
  planCreatedAt: string;
  milestoneId: string;
  milestoneTitle: string;
  milestoneCreatedAt: string;
};

const EMPTY_WORK_CREATE_FIELDS: WorkCreateFields = {
  taskId: '',
  taskTitle: '',
  taskCreatedAt: '',
  taskUpdatedAt: '',
  effort: '',
  initiativeId: '',
  initiativeTitle: '',
  initiativeCreatedAt: '',
  planId: '',
  planTitle: '',
  planCreatedAt: '',
  milestoneId: '',
  milestoneTitle: '',
  milestoneCreatedAt: '',
};

function integer(value: string): number | undefined {
  if (value.trim() === '') return undefined;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : undefined;
}

function currentWorkSelection(
  graph: WorkResult<WorkGraphReadV1> | undefined,
): WorkProductSelectionScopeV1 | undefined {
  if (graph?.outcome !== 'value' || graph.value.mode !== 'current') return undefined;
  return graph.value.authorized_scope.selection;
}

function selectionDetail(selection: WorkProductSelectionScopeV1 | undefined): string {
  if (selection === undefined) return 'A current graph read is required before preparation.';
  if (selection.selection === 'profile_owned_no_git') return 'Profile-owned Work graph with no Git relation scope.';
  return `${selection.relation_scopes.length} authorized relation scope${selection.relation_scopes.length === 1 ? '' : 's'}.`;
}

function workCreateDraft(
  fields: WorkCreateFields,
  selection: WorkProductSelectionScopeV1 | undefined,
): PrepareWorkProductMutationRequestV1 | undefined {
  const taskCreatedAt = integer(fields.taskCreatedAt);
  const taskUpdatedAt = integer(fields.taskUpdatedAt);
  const effort = integer(fields.effort);
  const initiativeCreatedAt = integer(fields.initiativeCreatedAt);
  const planCreatedAt = integer(fields.planCreatedAt);
  const milestoneCreatedAt = integer(fields.milestoneCreatedAt);
  if (
    selection === undefined ||
    taskCreatedAt === undefined ||
    taskUpdatedAt === undefined ||
    effort === undefined ||
    initiativeCreatedAt === undefined ||
    planCreatedAt === undefined ||
    milestoneCreatedAt === undefined ||
    fields.taskId.trim() === '' ||
    fields.taskTitle.trim() === '' ||
    fields.initiativeId.trim() === '' ||
    fields.initiativeTitle.trim() === '' ||
    fields.planId.trim() === '' ||
    fields.planTitle.trim() === '' ||
    fields.milestoneId.trim() === '' ||
    fields.milestoneTitle.trim() === ''
  ) {
    return undefined;
  }

  return {
    causation_event_id: null,
    evidence: [],
    selection,
    change: {
      change: 'create_task',
      initiative: {
        id: fields.initiativeId.trim(),
        title: fields.initiativeTitle.trim(),
        created_at: initiativeCreatedAt,
      },
      plan: {
        id: fields.planId.trim(),
        initiative_id: fields.initiativeId.trim(),
        title: fields.planTitle.trim(),
        created_at: planCreatedAt,
      },
      milestone: {
        id: fields.milestoneId.trim(),
        plan_id: fields.planId.trim(),
        title: fields.milestoneTitle.trim(),
        created_at: milestoneCreatedAt,
      },
      item: {
        accepted_at: null,
        accepted_attempts: [],
        accepted_criteria: {},
        accepted_proposal: null,
        accepted_route: null,
        archived_at: null,
        evidence_links: [],
        execution_admitted_at: null,
        handoffs: [],
        input: {
          acceptance_criteria: [],
          causal_candidates: [],
          created_at: taskCreatedAt,
          deadline: null,
          dependencies: [],
          effort,
          hierarchy: {
            initiative_id: fields.initiativeId.trim(),
            plan_id: fields.planId.trim(),
            milestone_id: fields.milestoneId.trim(),
          },
          informational_relations: [],
          scheduled_at: null,
          task_id: fields.taskId.trim(),
          title: fields.taskTitle.trim(),
          updated_at: taskUpdatedAt,
        },
      },
    },
  };
}

function mutationReading<T>(
  result: WorkResult<T> | undefined,
  pending: boolean,
  ready: string,
): { state: DomainStateKind; detail: string } | undefined {
  if (pending) return { state: 'loading', detail: 'asking the canonical Work authority' };
  if (result === undefined) return undefined;
  return result.outcome === 'value'
    ? { state: 'ready', detail: ready }
    : { state: result.state, detail: result.detail };
}

function CreateField({
  label,
  value,
  onChange,
  mono = false,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  mono?: boolean;
}) {
  const id = `work-create-${label.toLowerCase().replaceAll(/[^a-z0-9]+/g, '-')}`;
  return (
    <label className="flex flex-col gap-0.5 text-2xs text-text-secondary" htmlFor={id}>
      {label}
      <input
        id={id}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        className={`min-h-[44px] rounded-sm border border-edge bg-surface-1 px-2 text-2xs text-text-primary focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent${mono ? ' font-mono' : ''}`}
      />
    </label>
  );
}

/**
 * Task creation is a prepare/review/mutate handoff. The browser supplies only
 * operator-entered hierarchy data and the exact selection returned by the
 * current graph read. The Work authority mints every command identity, clock,
 * graph-version CAS, and revision pin before the second request reuses it.
 */
export function WorkCreate({ graph }: { graph: WorkResult<WorkGraphReadV1> | undefined }) {
  const prepare = useWorkReadAction(WORK_PREPARE_GRAPH_MUTATION_ROUTE);
  const mutate = useWorkCommand(WORK_MUTATE_GRAPH_ROUTE);
  const [fields, setFields] = useState<WorkCreateFields>(EMPTY_WORK_CREATE_FIELDS);
  const [prepared, setPrepared] = useState<
    { draftKey: string; mutation: WorkProductMutationRequestV1 } | undefined
  >(undefined);
  const [stalePreparedKey, setStalePreparedKey] = useState<string | undefined>(undefined);
  const selection = currentWorkSelection(graph);
  const draft = workCreateDraft(fields, selection);
  const draftKey = draft === undefined ? undefined : JSON.stringify(draft);
  const preparedMutation =
    draftKey !== undefined &&
    prepared?.draftKey === draftKey &&
    prepared.mutation.mutation === 'create_task'
      ? prepared.mutation
      : undefined;
  const prepareState = mutationReading(
    prepare.data,
    prepare.isPending,
    'the canonical Work authority prepared the exact create-task command',
  );
  const mutationState = mutationReading(
    mutate.data,
    mutate.isPending,
    'the canonical Work authority committed the prepared task',
  );

  async function prepareCreate(): Promise<void> {
    if (draft === undefined || draftKey === undefined) return;
    setPrepared(undefined);
    setStalePreparedKey(undefined);
    const result = await prepare.mutateAsync(draft);
    if (result.outcome === 'value' && result.value.mutation === 'create_task') {
      setPrepared({ draftKey, mutation: result.value });
    }
  }

  async function mutateCreate(): Promise<void> {
    if (preparedMutation === undefined || stalePreparedKey === prepared?.draftKey) return;
    const result = await mutate.mutateAsync(preparedMutation);
    if (result.outcome === 'refused' && result.state === 'conflicting') {
      setStalePreparedKey(prepared?.draftKey);
    }
  }

  const receipt = mutate.data?.outcome === 'value' ? mutate.data.value : undefined;

  return (
    <Panel legend="Create work" bodyClassName="p-0">
      <form
        className="flex flex-col gap-1.5 px-2 py-1.5"
        onSubmit={(event) => {
          event.preventDefault();
          void prepareCreate();
        }}
      >
        <p className="text-3xs text-text-muted">
          Task relations, evidence, acceptance criteria, and scheduling start empty. UTC microseconds and declared hierarchy are explicit so this browser never fabricates authority data.
        </p>
        <div className="rounded-sm border border-edge bg-surface-2 px-2 py-1">
          <p className="text-2xs text-text-secondary">Current graph selection</p>
          <p className="text-3xs text-text-muted">{selectionDetail(selection)}</p>
        </div>
        <CreateField label="Task identity" value={fields.taskId} mono onChange={(taskId) => setFields((value) => ({ ...value, taskId }))} />
        <CreateField label="Task title" value={fields.taskTitle} onChange={(taskTitle) => setFields((value) => ({ ...value, taskTitle }))} />
        <CreateField label="Task created at (UTC microseconds)" value={fields.taskCreatedAt} mono onChange={(taskCreatedAt) => setFields((value) => ({ ...value, taskCreatedAt }))} />
        <CreateField label="Task updated at (UTC microseconds)" value={fields.taskUpdatedAt} mono onChange={(taskUpdatedAt) => setFields((value) => ({ ...value, taskUpdatedAt }))} />
        <CreateField label="Task effort" value={fields.effort} mono onChange={(effort) => setFields((value) => ({ ...value, effort }))} />
        <CreateField label="Initiative identity" value={fields.initiativeId} mono onChange={(initiativeId) => setFields((value) => ({ ...value, initiativeId }))} />
        <CreateField label="Initiative title" value={fields.initiativeTitle} onChange={(initiativeTitle) => setFields((value) => ({ ...value, initiativeTitle }))} />
        <CreateField label="Initiative created at (UTC microseconds)" value={fields.initiativeCreatedAt} mono onChange={(initiativeCreatedAt) => setFields((value) => ({ ...value, initiativeCreatedAt }))} />
        <CreateField label="Plan identity" value={fields.planId} mono onChange={(planId) => setFields((value) => ({ ...value, planId }))} />
        <CreateField label="Plan title" value={fields.planTitle} onChange={(planTitle) => setFields((value) => ({ ...value, planTitle }))} />
        <CreateField label="Plan created at (UTC microseconds)" value={fields.planCreatedAt} mono onChange={(planCreatedAt) => setFields((value) => ({ ...value, planCreatedAt }))} />
        <CreateField label="Milestone identity" value={fields.milestoneId} mono onChange={(milestoneId) => setFields((value) => ({ ...value, milestoneId }))} />
        <CreateField label="Milestone title" value={fields.milestoneTitle} onChange={(milestoneTitle) => setFields((value) => ({ ...value, milestoneTitle }))} />
        <CreateField label="Milestone created at (UTC microseconds)" value={fields.milestoneCreatedAt} mono onChange={(milestoneCreatedAt) => setFields((value) => ({ ...value, milestoneCreatedAt }))} />
        <div className="flex flex-wrap items-center justify-between gap-2">
          <button
            type="submit"
            disabled={draft === undefined || prepare.isPending}
            className="min-h-[44px] rounded-sm border border-edge px-2 py-1 text-2xs text-text-primary hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:text-text-muted"
          >
            Prepare task creation
          </button>
          {prepareState === undefined ? null : <StateChip kind={prepareState.state} detail={prepareState.detail} />}
        </div>
      </form>

      {preparedMutation === undefined ? null : (
        <section className="border-t border-edge px-2 py-1.5" aria-label="Prepared canonical mutation">
          <h3 className="text-2xs text-text-primary">Prepared canonical mutation</h3>
          <dl className="mt-1 grid gap-0.5 text-3xs text-text-muted">
            <div><dt className="inline text-text-secondary">Selection: </dt><dd className="inline">{selectionDetail(preparedMutation.request.selection)}</dd></div>
            <div><dt className="inline text-text-secondary">Task: </dt><dd className="inline font-mono">{preparedMutation.request.item.input.task_id}</dd></div>
            <div><dt className="inline text-text-secondary">Hierarchy: </dt><dd className="inline font-mono">{preparedMutation.request.initiative.id} / {preparedMutation.request.plan.id} / {preparedMutation.request.milestone.id}</dd></div>
            <div><dt className="inline text-text-secondary">Exact command: </dt><dd className="inline font-mono">{preparedMutation.request.mutation.command_id}</dd></div>
            <div><dt className="inline text-text-secondary">Expected graph authority: </dt><dd className="inline font-mono">{preparedMutation.request.mutation.expected_authority.authority}</dd></div>
            <div><dt className="inline text-text-secondary">Revision pins: </dt><dd className="inline font-mono">{preparedMutation.request.mutation.revisions.policy_revision_id} · {preparedMutation.request.mutation.revisions.configuration_revision_id} · {preparedMutation.request.mutation.revisions.catalog_generation_id}</dd></div>
          </dl>
          <div className="mt-1 flex flex-wrap items-center justify-between gap-2">
            <button
              type="button"
              onClick={() => void mutateCreate()}
              disabled={mutate.isPending || stalePreparedKey === prepared?.draftKey}
              className="min-h-[44px] rounded-sm border border-edge px-2 py-1 text-2xs text-text-primary hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:text-text-muted"
            >
              Create prepared task
            </button>
            {mutationState === undefined ? null : <StateChip kind={mutationState.state} detail={mutationState.detail} />}
          </div>
          {stalePreparedKey === prepared?.draftKey ? (
            <p className="mt-1 text-3xs text-text-muted">The prepared command is stale. Prepare again from the current graph.</p>
          ) : null}
        </section>
      )}

      {receipt === undefined ? null : <WorkCreateReceipt receipt={receipt} />}
    </Panel>
  );
}

function WorkCreateReceipt({ receipt }: { receipt: WorkProductMutationReceiptV1 }) {
  return (
    <section className="border-t border-edge px-2 py-1.5" aria-label="Current graph receipt">
      <h3 className="text-2xs text-text-primary">Current graph receipt</h3>
      <p className="mt-1 font-mono text-3xs text-text-muted">
        {receipt.event.event_id} · graph version {receipt.verified_graph_version.graph_version} · sequence {receipt.verified_graph_version.event_sequence} · {receipt.replayed ? 'replayed' : 'committed'}
      </p>
    </section>
  );
}
