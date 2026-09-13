import { useSearchParams } from 'react-router';
import { StateChip } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import {
  WORK_STAGES,
  coverageReading,
  stageLabel,
  stageState,
  workStage,
  type WorkStage,
} from './workModel.ts';
import {
  graphRuntimeAttempts,
  graphEntryOf,
  terminalWorkAttempt,
  type WorkGraphReading,
} from './workGraphModel.ts';
import type { WorkProductView, WorkTaskView } from './workProductView.ts';

/**
 * The Work board.
 *
 * Grouped by stage rather than laid out as lanes. The difference is not
 * cosmetic: a lane needs a status field to sort tasks into, `WorkProjection`
 * has none, and the stage each group here names is read directly off
 * `accepted_proposal`, `task_accepted`, `execution_admitted` and terminal
 * attempts in the exact Work graph. Every column below is likewise a field the
 * daemon sent — there is no derived progress, no elapsed time, no health.
 *
 * Empty groups are drawn, with a count of zero. A stage with no tasks in it is
 * a fact the snapshot supports; hiding it would leave a reader unable to tell an
 * empty stage from one this build does not know about.
 */

/** The query parameter that selects a task, so a board position survives a
 * reload and can be linked to. */
export const TASK_PARAM = 'task';

export function useSelectedTask(): [string | null, (taskId: string | null) => void] {
  const [params, setParams] = useSearchParams();
  const selected = params.get(TASK_PARAM);
  const select = (taskId: string | null) => {
    const next = new URLSearchParams(params);
    if (taskId === null) next.delete(TASK_PARAM);
    else next.set(TASK_PARAM, taskId);
    // Replace rather than push: moving along a list of tasks should not fill
    // the back button with every row visited on the way.
    setParams(next, { replace: true });
  };
  return [selected, select];
}

function AttemptCoverage({ graph }: { graph: WorkGraphReading }) {
  if (graph.state === 'pending') {
    return (
      <span data-work-board-attempts="pending">
        <StateChip kind="loading" detail="reading exact graph attempts" />
      </span>
    );
  }
  if (graph.state === 'refused') {
    return (
      <span data-work-board-attempts="refused">
        <StateChip kind={graph.chip} detail={graph.detail} />
      </span>
    );
  }
  const entry = graphEntryOf(graph);
  if (entry === null) {
    return (
      <span data-work-board-attempts="absent">
        <StateChip kind="complete_zero_findings" detail="no graph version in the window" />
      </span>
    );
  }
  const coverage = entry.runtime.coverage;
  if (coverage.coverage === 'unavailable') {
    return (
      <span data-work-board-attempts="unavailable">
        <StateChip kind="unavailable" detail="attempt stages are unmeasured, not zero" />
      </span>
    );
  }
  const returned = entry.runtime.attempts.length;
  if (coverage.coverage === 'partial') {
    return (
      <span data-work-board-attempts="partial">
        <StateChip
          kind="partial"
          detail={`${returned} returned · ${coverage.unavailable_attempts.length} unavailable · graph v${entry.runtime.graph_version}`}
        />
      </span>
    );
  }
  return (
    <span data-work-board-attempts="complete">
      <StateChip kind="ready" detail={`${returned} attempts · graph v${entry.runtime.graph_version}`} />
    </span>
  );
}

function byStage(
  projections: readonly WorkTaskView[],
  graph: WorkGraphReading,
): ReadonlyMap<WorkStage, readonly WorkTaskView[]> {
  const grouped = new Map<WorkStage, WorkTaskView[]>(WORK_STAGES.map((stage) => [stage, []]));
  const terminal = new Set(
    graphRuntimeAttempts(graph)
      .filter((attempt) => terminalWorkAttempt(attempt.state))
      .map((attempt) => attempt.identity.task_id),
  );
  for (const projection of projections) {
    grouped.get(workStage(projection, terminal.has(projection.task_id)))?.push(projection);
  }
  return grouped;
}

function TaskRow({
  projection,
  attempts,
  selected,
  onSelect,
}: {
  projection: WorkTaskView;
  attempts: number;
  selected: boolean;
  onSelect: (taskId: string) => void;
}) {
  return (
    <tr
      data-work-task={projection.task_id}
      data-selected={selected ? 'true' : undefined}
      className={
        selected
          ? 'bg-surface-3 outline outline-1 -outline-offset-1 outline-accent'
          : 'hover:bg-surface-2'
      }
    >
      <th scope="row" className="px-2 py-1.5 text-left font-medium text-text-primary">
        {/* A button rather than a row click handler, so the row is reachable by
          * keyboard and announced as the control it is. */}
        {/* 44px explicitly, not `min-h-11`: this app's root font size is 14px,
          * so a spacing-11 minimum computes to 38.5px and lands under the
          * target size the gate measures. */}
        <button
          type="button"
          onClick={() => onSelect(projection.task_id)}
          aria-pressed={selected}
          className="flex min-h-[44px] w-full items-center text-left underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          {projection.title}
        </button>
        {/* The identity and dependency count follow the title below `md`,
          * where their own columns are not drawn. Hiding a column with
          * `display:none` would take it out of the accessibility tree too, and
          * these appear nowhere else on the page. */}
        <span className="mt-0.5 block font-normal text-text-muted md:hidden">
          {projection.task_id} · v{projection.version} · {projection.dependencies.length} dep
          {projection.dependencies.length === 1 ? '' : 's'}
        </span>
      </th>
      <td className="px-2 py-1.5 font-mono text-text-secondary max-md:hidden">
        {projection.task_id}
      </td>
      <td className="px-2 py-1.5 text-right tabular-nums text-text-secondary max-md:hidden">
        {projection.version}
      </td>
      <td className="px-2 py-1.5 text-right tabular-nums text-text-secondary max-md:hidden">
        {projection.dependencies.length}
      </td>
      <td className="px-2 py-1.5 text-right tabular-nums text-text-secondary">
        {projection.history_len ?? '—'}
      </td>
      <td className="px-2 py-1.5 text-right tabular-nums text-text-secondary">
        {attempts}
      </td>
    </tr>
  );
}

function StageGroup({
  stage,
  projections,
  attemptsByTask,
  selected,
  onSelect,
}: {
  stage: WorkStage;
  projections: readonly WorkTaskView[];
  attemptsByTask: ReadonlyMap<string, number>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const legend = stageLabel(stage);
  return (
    <Panel
      legend={legend}
      actions={<StateChip kind={stageState(stage)} detail={`${projections.length}`} />}
      bodyClassName="p-0"
    >
      <div
        role="region"
        aria-label={`${legend} tasks`}
        tabIndex={0}
        className="min-w-0 overflow-x-auto"
        data-work-stage={stage}
      >
        <table className="w-full min-w-0 border-collapse text-2xs">
          <caption className="sr-only">
            Tasks whose furthest recorded gate is {legend.toLowerCase()}, with the identity,
            version, dependency count and history length from the snapshot, plus the attempts in the
            exact Work graph.
          </caption>
          <thead>
            <tr className="border-b border-edge text-text-muted">
              <th scope="col" className="px-2 py-1 text-left font-medium">
                Task
              </th>
              <th scope="col" className="px-2 py-1 text-left font-medium max-md:hidden">
                Identity
              </th>
              <th scope="col" className="px-2 py-1 text-right font-medium max-md:hidden">
                Version
              </th>
              <th scope="col" className="px-2 py-1 text-right font-medium max-md:hidden">
                Deps
              </th>
              <th scope="col" className="px-2 py-1 text-right font-medium">
                History
              </th>
              <th scope="col" className="px-2 py-1 text-right font-medium">
                Attempts
              </th>
            </tr>
          </thead>
          <tbody>
            {projections.length === 0 ? (
              <tr>
                <td colSpan={6} className="px-2 py-1.5 text-text-muted">
                  No task in this build has reached this gate and no further.
                </td>
              </tr>
            ) : (
              projections.map((projection) => (
                <TaskRow
                  key={projection.task_id}
                  projection={projection}
                  attempts={attemptsByTask.get(projection.task_id) ?? 0}
                  selected={projection.task_id === selected}
                  onSelect={onSelect}
                />
              ))
            )}
          </tbody>
        </table>
      </div>
    </Panel>
  );
}

export function WorkBoard({
  snapshot,
  graph,
  selected,
  onSelect,
}: {
  snapshot: WorkProductView;
  graph: WorkGraphReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const grouped = byStage(snapshot.projections, graph);
  const attemptsByTask = new Map<string, number>();
  for (const attempt of graphRuntimeAttempts(graph)) {
    attemptsByTask.set(
      attempt.identity.task_id,
      (attemptsByTask.get(attempt.identity.task_id) ?? 0) + 1,
    );
  }
  const coverage = coverageReading(snapshot.coverage);

  return (
    <div className="flex min-w-0 flex-col gap-3" data-work-board="snapshot">
      <div className="flex flex-wrap items-center gap-2 text-3xs text-text-muted">
        <StateChip kind={coverage.state} detail={coverage.detail} />
        <AttemptCoverage graph={graph} />
        {/* Sequence and generation are the snapshot's identity. Printed because
          * they are what makes a later delta legible, and because a board with
          * no stated position cannot be told apart from a stale one. */}
        <span className="font-mono">
          sequence {snapshot.sequence} · generation {snapshot.generation_id}
        </span>
      </div>
      <div className="grid min-w-0 gap-3 xl:grid-cols-2">
        {WORK_STAGES.map((stage) => (
          <StageGroup
            key={stage}
            stage={stage}
            projections={grouped.get(stage) ?? []}
            attemptsByTask={attemptsByTask}
            selected={selected}
            onSelect={onSelect}
          />
        ))}
      </div>
    </div>
  );
}
