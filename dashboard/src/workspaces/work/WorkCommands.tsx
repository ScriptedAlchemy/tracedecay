import { useState } from 'react';
import type { WorkProjection, WorkProjectionSnapshotV1 } from '../../contracts/index.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import type { WorkResult } from './workApi.ts';
import { useWorkCommand } from './workQueries.ts';
import {
  WORK_ACCEPT_TASK_ROUTE,
  WORK_ADMIT_EXECUTION_ROUTE,
  WORK_CREATE_ROUTE,
  WORK_REPLAN_DEPENDENCIES_ROUTE,
} from './workRoutes.ts';
import { availableCommands, commandBlocked, type WorkCommandKind } from './workModel.ts';

/**
 * The controls for one task.
 *
 * Two rules shape this panel. A control is drawn only where the task has
 * reached the gate the command acts on, and only where this build can assemble
 * the command from a generated read model — the second is why proposal review,
 * proposal acceptance and evidence attachment appear as stated gaps rather than
 * as buttons. Their reasons come from `commandBlocked`, so the explanation and
 * the decision not to draw them cannot drift apart.
 *
 * Every command carries the projection's own `version` as `expected_version`.
 * That is what makes them compare-and-swap: if the task moved since the board
 * was read, the daemon answers 409 and the control says the task moved rather
 * than retrying over the top of someone else's change.
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

const COMMAND_LABEL: Record<WorkCommandKind, string> = {
  replan_dependencies: 'Replan dependencies',
  review_proposal: 'Review proposal',
  accept_proposal: 'Accept proposal',
  accept_task: 'Accept task',
  admit_execution: 'Admit execution',
  attach_runtime_evidence: 'Attach runtime evidence',
};

/** What a finished command attempt reads as. `undefined` while nothing has been
 * sent, so a control that has never run says nothing rather than saying it
 * succeeded. */
function commandReading(
  result: WorkResult<WorkProjection> | undefined,
  pending: boolean,
): { state: 'loading' | 'ready' | 'conflicting' | 'error'; detail: string } | undefined {
  if (pending) return { state: 'loading', detail: 'sending' };
  if (result === undefined) return undefined;
  if (result.outcome === 'value') {
    return { state: 'ready', detail: `committed at version ${result.value.version}` };
  }
  return {
    state: result.state === 'conflicting' ? 'conflicting' : 'error',
    detail: result.detail,
  };
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
        <StateChip
          kind={reading.state === 'loading' ? 'loading' : reading.state}
          detail={reading.detail}
        />
      )}
    </div>
  );
}

function BlockedCommand({ label, reason }: { label: string; reason: string }) {
  return (
    <div className="border-b border-edge px-2 py-1.5 last:border-b-0" data-work-blocked={label}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-2xs text-text-muted">{label}</span>
        <StateChip kind="unsupported" detail="no input in this build" />
      </div>
      <p className="mt-0.5 text-3xs text-text-muted">{reason}</p>
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
  const admitExecution = useWorkCommand(WORK_ADMIT_EXECUTION_ROUTE);
  const replan = useWorkCommand(WORK_REPLAN_DEPENDENCIES_ROUTE);
  const [dependencies, setDependencies] = useState<readonly string[]>(projection.dependencies);

  const offered = availableCommands(projection);
  const candidates = snapshot.projections
    .map((candidate) => candidate.task_id)
    .filter((taskId) => taskId !== projection.task_id);

  return (
    <Panel legend={`Commands · ${projection.title}`} bodyClassName="p-0">
      <dl className="sr-only">
        <dt>Task</dt>
        <dd>{projection.task_id}</dd>
        <dt>Version used for compare-and-swap</dt>
        <dd>{projection.version}</dd>
      </dl>

      {offered.includes('accept_task') ? (
        <CommandButton
          label={COMMAND_LABEL.accept_task}
          disabled={acceptTask.isPending}
          reading={commandReading(acceptTask.data, acceptTask.isPending)}
          onRun={() =>
            acceptTask.mutate({
              command_id: commandId(),
              expected_version: projection.version,
              occurred_at: occurredAt(),
              task_id: projection.task_id,
            })
          }
        />
      ) : null}

      {offered.includes('admit_execution') ? (
        <CommandButton
          label={COMMAND_LABEL.admit_execution}
          disabled={admitExecution.isPending}
          reading={commandReading(admitExecution.data, admitExecution.isPending)}
          onRun={() =>
            admitExecution.mutate({
              command_id: commandId(),
              expected_version: projection.version,
              occurred_at: occurredAt(),
              task_id: projection.task_id,
            })
          }
        />
      ) : null}

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
            label={COMMAND_LABEL.replan_dependencies}
            disabled={replan.isPending}
            reading={commandReading(replan.data, replan.isPending)}
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

      {offered
        .map((kind) => ({ kind, reason: commandBlocked(kind) }))
        .filter(
          (entry): entry is { kind: WorkCommandKind; reason: string } => entry.reason !== undefined,
        )
        .map(({ kind, reason }) => (
          <BlockedCommand key={kind} label={COMMAND_LABEL[kind]} reason={reason} />
        ))}
    </Panel>
  );
}

/**
 * Creating a task.
 *
 * The one command with no `expected_version`, because a task that does not
 * exist has no version to compare against. Both of its inputs are genuinely the
 * operator's — a title and an identity — rather than opaque values lifted from
 * a read model this build does not have.
 */
export function WorkCreate() {
  const create = useWorkCommand(WORK_CREATE_ROUTE);
  const [taskId, setTaskId] = useState('');
  const [title, setTitle] = useState('');
  const ready = taskId.trim() !== '' && title.trim() !== '';

  return (
    <Panel legend="Create work" bodyClassName="p-0">
      <form
        className="flex flex-col gap-1.5 px-2 py-1.5"
        onSubmit={(event) => {
          event.preventDefault();
          if (!ready) return;
          create.mutate({
            command_id: commandId(),
            dependencies: [],
            occurred_at: occurredAt(),
            task_id: taskId.trim(),
            title: title.trim(),
          });
        }}
      >
        <label htmlFor="work-create-task-id" className="text-2xs text-text-secondary">
          Task identity
        </label>
        <input
          id="work-create-task-id"
          value={taskId}
          onChange={(event) => setTaskId(event.target.value)}
          className="min-h-[44px] rounded-sm border border-edge bg-surface-1 px-2 font-mono text-2xs text-text-primary focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        />
        <label htmlFor="work-create-title" className="text-2xs text-text-secondary">
          Title
        </label>
        <input
          id="work-create-title"
          value={title}
          onChange={(event) => setTitle(event.target.value)}
          className="min-h-[44px] rounded-sm border border-edge bg-surface-1 px-2 text-2xs text-text-primary focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        />
        <div className="flex flex-wrap items-center justify-between gap-2">
          <button
            type="submit"
            disabled={!ready || create.isPending}
            className="min-h-[44px] rounded-sm border border-edge px-2 py-1 text-2xs text-text-primary hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:text-text-muted"
          >
            Create
          </button>
          {commandReading(create.data, create.isPending) === undefined ? null : (
            <StateChip
              kind={
                commandReading(create.data, create.isPending)?.state === 'loading'
                  ? 'loading'
                  : (commandReading(create.data, create.isPending)?.state ?? 'error')
              }
              detail={commandReading(create.data, create.isPending)?.detail}
            />
          )}
        </div>
      </form>
    </Panel>
  );
}
