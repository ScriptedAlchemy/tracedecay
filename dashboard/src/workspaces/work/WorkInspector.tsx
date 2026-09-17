import type { ReactNode } from 'react';
import type {
  ExecutionTopologyMetricsV1,
  ExecutionTopologyViewV1,
  WorkGraphReadV1,
} from '../../contracts/index.ts';
import { EvidenceGrade, type EvidenceGradeKind } from '../../ui/EvidenceGrade.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { cn } from '../../ui/cn.ts';
import type { WorkResult } from './workApi.ts';
import { WorkCommands } from './WorkCommands.tsx';
import { WorkEvidencePanel } from './WorkEvidencePanel.tsx';
import {
  graphEntryOf,
  graphRuntimeAttempts,
  type WorkGraphReading,
} from './workGraphModel.ts';
import { laneReading } from './workLaneModel.ts';
import type { WorkProductView, WorkTaskView } from './workProductView.ts';
import type { WorkDagReading } from './workViewsModel.ts';

/**
 * The Work inspector — the workspace-owned column that opens on selection.
 *
 * Selection opens it without mutating the graph. It keeps task definition,
 * admission, placement, attempts, and evidence in separate registers so no
 * one status stands in for another: a lane is not an attempt, an accepted
 * proposal is not an admitted execution, and a command that the daemon has
 * not prepared is not a command.
 *
 * Every row names its grade. Fields the concept plate pictures but no Work
 * authority publishes — priority, an owner field — are printed as the typed
 * absences they are rather than filled from a neighbouring field.
 */

export function WorkInspector({
  task,
  snapshot,
  graph,
  graphReading,
  dag,
  topology,
  topologyMetrics,
  onSelect,
}: {
  task: WorkTaskView | null;
  snapshot: WorkProductView;
  graph: WorkResult<WorkGraphReadV1> | undefined;
  graphReading: WorkGraphReading;
  dag: WorkDagReading;
  topology: WorkResult<ExecutionTopologyViewV1> | undefined;
  topologyMetrics: WorkResult<ExecutionTopologyMetricsV1> | undefined;
  onSelect: (taskId: string) => void;
}) {
  if (task === null) {
    return (
      <aside
        aria-label="Work inspector"
        className="flex min-w-0 flex-col gap-3"
        data-work-inspector="empty"
      >
        <Panel legend="Selected task">
          <p className="text-2xs leading-relaxed text-text-muted">
            Select a task from the graph, the board, or the exact table. Hover and focus inspect
            a neighbourhood without changing the selection; the inspector opens on click or
            Enter and never mutates the graph.
          </p>
        </Panel>
        <Panel legend="Production commands">
          <p className="text-2xs text-text-muted">
            Commands are prepared by the daemon for one selected task. Nothing is offered until
            a task is selected and the canonical Work authority has answered.
          </p>
        </Panel>
        <WorkEvidenceSummary
          task={null}
          snapshot={snapshot}
          graph={graph}
          graphReading={graphReading}
          topologyMetrics={topologyMetrics}
        />
      </aside>
    );
  }

  return (
    <aside
      aria-label="Work inspector"
      className="flex min-w-0 flex-col gap-3"
      data-work-inspector={task.task_id}
    >
      <SelectedTask task={task} dag={dag} graphReading={graphReading} topology={topology} onSelect={onSelect} />
      <WorkCommands projection={task} graph={graph} />
      <WorkEvidenceSummary
        task={task}
        snapshot={snapshot}
        graph={graph}
        graphReading={graphReading}
        topologyMetrics={topologyMetrics}
      />
      <WorkEvidencePanel taskId={task.task_id} graph={graph} />
    </aside>
  );
}

/** One inspector row: an engraved term, its value, and the grade the value
 * carries. Values wrap rather than truncate — an identity a reader cannot
 * read in full is not an identity. */
function Row({
  term,
  grade,
  source,
  children,
  muted = false,
}: {
  term: string;
  grade: EvidenceGradeKind;
  source: string;
  children: ReactNode;
  muted?: boolean;
}) {
  return (
    <div className="grid min-w-0 grid-cols-[minmax(5.5rem,auto)_minmax(0,1fr)] items-baseline gap-x-3 gap-y-0.5 border-b border-edge-subtle py-1.5 last:border-b-0">
      <dt className="td-legend pt-px">{term}</dt>
      <dd className={cn('flex min-w-0 flex-col gap-0.5 text-2xs', muted ? 'text-text-muted' : 'text-text-secondary')}>
        <span className="min-w-0 break-words">{children}</span>
        <EvidenceGrade grade={grade} source={source} />
      </dd>
    </div>
  );
}

function TaskLinks({
  ids,
  onSelect,
  empty,
}: {
  ids: readonly string[];
  onSelect: (taskId: string) => void;
  empty: string;
}) {
  if (ids.length === 0) return <span className="text-text-muted">{empty}</span>;
  return (
    <span className="flex min-w-0 flex-wrap gap-x-2 gap-y-0.5">
      {ids.map((id) => (
        <button
          key={id}
          type="button"
          onClick={() => onSelect(id)}
          className="td-value min-h-[44px] min-w-0 truncate text-2xs text-text-secondary underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          {id}
        </button>
      ))}
    </span>
  );
}

function SelectedTask({
  task,
  dag,
  graphReading,
  topology,
  onSelect,
}: {
  task: WorkTaskView;
  dag: WorkDagReading;
  graphReading: WorkGraphReading;
  topology: WorkResult<ExecutionTopologyViewV1> | undefined;
  onSelect: (taskId: string) => void;
}) {
  const lane = laneReading(task.lane);
  const node = dag.nodes.get(task.task_id);
  const actors = [...new Set(task.handoffs.flatMap((handoff) => [handoff.fromActor, handoff.toActor]))];
  const attempts = graphRuntimeAttempts(graphReading).filter(
    (attempt) => attempt.identity.task_id === task.task_id,
  );
  const entry = graphEntryOf(graphReading);
  const runtimeCoverage = entry?.runtime.coverage.coverage ?? null;

  return (
    <Panel legend="Selected task" bodyClassName="p-0">
      <div className="flex min-w-0 flex-col gap-1 border-b border-edge-subtle px-3 py-2.5">
        <span className="td-value text-3xs text-text-muted">{task.task_id}</span>
        <h3 className="min-w-0 break-words text-sm text-text-primary">{task.title}</h3>
        <span className="flex items-center gap-2" data-work-inspector-lane={task.lane.kind === 'projected' ? task.lane.lane : 'uncarded'}>
          <span
            aria-hidden
            className={cn('size-1.5', lane.swatch ?? 'border border-dashed border-text-muted bg-transparent')}
          />
          <span className="td-legend text-text-secondary">{lane.label}</span>
          <EvidenceGrade grade={task.lane.kind === 'projected' ? 'exact' : 'unavailable'} source="KANBAN" />
        </span>
        <p className="text-3xs leading-snug text-text-muted">{lane.sentence}</p>
      </div>

      <dl className="px-3 py-1" data-work-inspector-definition>
        <Row term="milestone" grade="exact" source="GRAPH">
          <span className="td-value">{task.hierarchy.milestone_id}</span>
        </Row>
        <Row term="plan" grade="exact" source="GRAPH">
          <span className="td-value">{task.hierarchy.plan_id}</span>
        </Row>
        <Row term="initiative" grade="exact" source="GRAPH">
          <span className="td-value">{task.hierarchy.initiative_id}</span>
        </Row>
        <Row term="effort" grade="exact" source="GRAPH">
          <span className="td-value" data-cell="numeric">
            {task.effort}
          </span>{' '}
          <span className="text-text-muted">declared effort · an integer, not a duration</span>
        </Row>
        <Row term="priority" grade="unavailable" source="GRAPH" muted>
          the Work graph declares no priority field for a task
        </Row>
        {actors.length === 0 ? (
          <Row term="owners" grade="unavailable" source="GRAPH" muted>
            no owner field is declared, and no handoff names an actor
          </Row>
        ) : (
          <Row term="actors" grade="explicit" source="GRAPH">
            <span className="td-value">{actors.join(', ')}</span>{' '}
            <span className="text-text-muted">
              named by {task.handoffs.length} recorded {task.handoffs.length === 1 ? 'handoff' : 'handoffs'}, not an owner field
            </span>
          </Row>
        )}
        <Row term="created" grade="exact" source="GRAPH">
          <span className="td-value">{formatMicrosUtc(task.created_at)}</span>
        </Row>
        <Row term="updated" grade="exact" source="GRAPH">
          <span className="td-value">{formatMicrosUtc(task.updated_at)}</span>
        </Row>
        <Row term="scheduled" grade="exact" source="GRAPH" muted={task.scheduled_at === null}>
          {task.scheduled_at === null ? (
            'not scheduled · the plan declares no schedule instant'
          ) : (
            <span className="td-value">{formatMicrosUtc(task.scheduled_at)}</span>
          )}
        </Row>
        <Row term="deadline" grade="exact" source="GRAPH" muted={task.deadline === null}>
          {task.deadline === null ? (
            'no deadline · the plan declares none'
          ) : (
            <span className="td-value">{formatMicrosUtc(task.deadline)}</span>
          )}
        </Row>
        <Row term="depth" grade="inferred" source="GRAPH">
          {node === undefined ? (
            <span className="text-text-muted">not layered · the task is outside the drawn page</span>
          ) : (
            <>
              <span className="td-value" data-cell="numeric">
                {node.depth}
              </span>{' '}
              <span className="text-text-muted">
                longest declared dependency path{node.cyclic ? ' · inside a declared cycle' : ''}
              </span>
            </>
          )}
        </Row>
      </dl>

      <section aria-label="Admission" className="border-t border-edge-subtle px-3 py-1" data-work-inspector-admission>
        <h4 className="td-legend pt-1.5 text-text-secondary">admission</h4>
        <dl>
          <Row term="proposal" grade="exact" source="GRAPH" muted={task.accepted_proposal === null}>
            {task.accepted_proposal === null ? (
              'no proposal accepted'
            ) : (
              <span className="td-value">{task.accepted_proposal}</span>
            )}
          </Row>
          <Row term="accepted" grade="exact" source="GRAPH" muted={task.accepted_at === null}>
            {task.accepted_at === null ? (
              'task not accepted'
            ) : (
              <span className="td-value">{formatMicrosUtc(task.accepted_at)}</span>
            )}
          </Row>
          <Row term="admitted" grade="exact" source="GRAPH" muted={task.execution_admitted_at === null}>
            {task.execution_admitted_at === null ? (
              'execution not admitted'
            ) : (
              <span className="td-value">{formatMicrosUtc(task.execution_admitted_at)}</span>
            )}
          </Row>
          {task.archived_at === null ? null : (
            <Row term="archived" grade="exact" source="GRAPH">
              <span className="td-value">{formatMicrosUtc(task.archived_at)}</span>
            </Row>
          )}
        </dl>
      </section>

      <section aria-label="Relations" className="border-t border-edge-subtle px-3 py-1" data-work-inspector-relations>
        <h4 className="td-legend pt-1.5 text-text-secondary">relations</h4>
        <dl>
          <Row term="gates on" grade="exact" source="GRAPH">
            <TaskLinks ids={task.dependencies} onSelect={onSelect} empty="no gating dependency declared" />
          </Row>
          <Row term="gates" grade="exact" source="GRAPH">
            <TaskLinks ids={node?.dependents ?? []} onSelect={onSelect} empty="no task on this page depends on it" />
          </Row>
          <Row term="informational" grade="explicit" source="GRAPH">
            <TaskLinks ids={task.informational_relations} onSelect={onSelect} empty="none declared" />
          </Row>
          <Row term="causal" grade="explicit" source="GRAPH">
            <TaskLinks ids={task.causal_candidates} onSelect={onSelect} empty="no candidate cause nominated" />
          </Row>
        </dl>
      </section>

      <section aria-label="Placement and attempts" className="border-t border-edge-subtle px-3 py-1" data-work-inspector-placement>
        <h4 className="td-legend pt-1.5 text-text-secondary">placement · attempts</h4>
        <dl>
          <PlacementRow taskId={task.task_id} topology={topology} />
          <Row
            term="attempts"
            grade={runtimeCoverage === null || runtimeCoverage === 'unavailable' ? 'unavailable' : runtimeCoverage === 'partial' ? 'ambiguous' : 'exact'}
            source="RUNTIME"
            muted={attempts.length === 0}
          >
            {runtimeCoverage === 'unavailable' ? (
              'the runtime projection could not be read · attempts are unmeasured, not zero'
            ) : attempts.length === 0 ? (
              'no attempt under this graph version'
            ) : (
              <span className="flex min-w-0 flex-col gap-0.5">
                {attempts.map((attempt) => (
                  <span key={attempt.identity.attempt_id} className="td-value min-w-0 break-words">
                    {attempt.identity.run_id} / {attempt.identity.attempt_id} · {attempt.state}
                  </span>
                ))}
                {runtimeCoverage === 'partial' ? (
                  <span className="text-text-muted">runtime coverage is partial · this list is a floor</span>
                ) : null}
              </span>
            )}
          </Row>
        </dl>
      </section>
    </Panel>
  );
}

function PlacementRow({
  taskId,
  topology,
}: {
  taskId: string;
  topology: WorkResult<ExecutionTopologyViewV1> | undefined;
}) {
  if (topology === undefined) {
    return (
      <Row term="placement" grade="unavailable" source="TOPOLOGY" muted>
        not read under this projection · the Topology camera issues the placement read
      </Row>
    );
  }
  if (topology.outcome === 'refused') {
    return (
      <Row term="placement" grade="unavailable" source="TOPOLOGY" muted>
        <StateChip kind={topology.state} detail={topology.detail} />
      </Row>
    );
  }
  if (topology.value.state === 'absent') {
    return (
      <Row term="placement" grade="unavailable" source="TOPOLOGY" muted>
        the topology authority reports no view in this scope
      </Row>
    );
  }
  const lanes = topology.value.execution_placement.lanes.filter((lane) => lane.task_id === taskId);
  if (lanes.length === 0) {
    return (
      <Row term="placement" grade="exact" source="TOPOLOGY" muted>
        no placement lane for this task on the read page
      </Row>
    );
  }
  return (
    <Row term="placement" grade="exact" source="TOPOLOGY">
      <span className="flex min-w-0 flex-col gap-0.5">
        {lanes.map((lane) => (
          <span key={`${lane.task_id}:${lane.run_id}`} className="td-value min-w-0 break-words">
            {lane.run_id} ·{' '}
            {lane.placement.state === 'absent'
              ? 'placement absent'
              : `${lane.placement.placement.state} · ${lane.placement.placement.target.kind}${lane.placement.placement.target.root === null ? '' : ` · ${lane.placement.placement.target.root}`}`}
          </span>
        ))}
      </span>
    </Row>
  );
}

function selectionScope(graph: WorkResult<WorkGraphReadV1> | undefined): {
  grade: EvidenceGradeKind;
  text: string;
} {
  if (graph === undefined) return { grade: 'unavailable', text: 'the graph read has not answered' };
  if (graph.outcome === 'refused') return { grade: 'unavailable', text: graph.detail };
  const selection = graph.value.authorized_scope.selection;
  if (selection.selection === 'profile_owned_no_git') {
    return { grade: 'exact', text: 'profile-owned · no Git relation scope' };
  }
  return {
    grade: 'exact',
    text: `${selection.relation_scopes.length} relation ${selection.relation_scopes.length === 1 ? 'scope' : 'scopes'} · ${selection.relation_scopes
      .map((scope) => (scope.kind === 'repository' ? scope.repository_id : scope.kind))
      .join(', ')}`,
  };
}

/**
 * The counts the concept plate lists under WORK EVIDENCE, each bound to the
 * read that supplied it. A count that no mounted read supplied under this
 * camera is stated as not read — never as zero.
 */
function WorkEvidenceSummary({
  task,
  snapshot,
  graph,
  graphReading,
  topologyMetrics,
}: {
  task: WorkTaskView | null;
  snapshot: WorkProductView;
  graph: WorkResult<WorkGraphReadV1> | undefined;
  graphReading: WorkGraphReading;
  topologyMetrics: WorkResult<ExecutionTopologyMetricsV1> | undefined;
}) {
  const scope = selectionScope(graph);
  const entry = graphEntryOf(graphReading);
  const runtimeCoverage = entry?.runtime.coverage ?? null;
  const runtimeAttempts =
    task === null
      ? graphRuntimeAttempts(graphReading)
      : graphRuntimeAttempts(graphReading).filter((attempt) => attempt.identity.task_id === task.task_id);
  return (
    <Panel legend="Work evidence" bodyClassName="p-0">
      <dl className="px-3 py-1" data-work-inspector-evidence>
        <Row term="criteria" grade={task === null ? 'unavailable' : 'exact'} source="GRAPH" muted={task === null}>
          {task === null ? (
            'select a task'
          ) : (
            <>
              <span className="td-value" data-cell="numeric">
                {task.acceptance_criteria_count}
              </span>{' '}
              <span className="text-text-muted">
                acceptance {task.acceptance_criteria_count === 1 ? 'criterion' : 'criteria'} · {task.evidence_links_count} evidence{' '}
                {task.evidence_links_count === 1 ? 'link' : 'links'}
                {task.acceptance_evidence_required ? ' · evidence required to accept' : ''}
              </span>
            </>
          )}
        </Row>
        <Row
          term="attempts"
          grade={
            runtimeCoverage === null || runtimeCoverage.coverage === 'unavailable'
              ? 'unavailable'
              : runtimeCoverage.coverage === 'partial'
                ? 'ambiguous'
                : 'exact'
          }
          source="RUNTIME"
          muted={runtimeCoverage === null || runtimeCoverage.coverage === 'unavailable'}
        >
          {runtimeCoverage === null ? (
            'the graph read has not produced a version'
          ) : runtimeCoverage.coverage === 'unavailable' ? (
            'unmeasured · the runtime projection could not be read'
          ) : (
            <>
              <span className="td-value" data-cell="numeric">
                {runtimeAttempts.length}
              </span>{' '}
              <span className="text-text-muted">
                runtime {runtimeAttempts.length === 1 ? 'attempt' : 'attempts'}
                {task === null ? ' across the graph' : ` · ${task.accepted_attempts_count} accepted`}
                {runtimeCoverage.coverage === 'partial'
                  ? ` · ${runtimeCoverage.unavailable_attempts.length} unavailable, so a floor`
                  : ''}
              </span>
            </>
          )}
        </Row>
        <Row term="graph version" grade="exact" source="GRAPH">
          <span className="td-value" data-cell="numeric">
            v{snapshot.graph_version} / seq {snapshot.event_sequence}
          </span>{' '}
          <span className="text-text-muted">immutable · observed {formatMicrosUtc(snapshot.observed_at)}</span>
        </Row>
        <TopologyMetricsRow metrics={topologyMetrics} />
        <Row term="selection scope" grade={scope.grade} source="GRAPH" muted={scope.grade === 'unavailable'}>
          {scope.text}
        </Row>
      </dl>
    </Panel>
  );
}

function TopologyMetricsRow({
  metrics,
}: {
  metrics: WorkResult<ExecutionTopologyMetricsV1> | undefined;
}) {
  if (metrics === undefined) {
    return (
      <Row term="topology metrics" grade="unavailable" source="TOPOLOGY" muted>
        not read under this projection · the Topology camera issues the accounting read
      </Row>
    );
  }
  if (metrics.outcome === 'refused') {
    return (
      <Row term="topology metrics" grade="unavailable" source="TOPOLOGY" muted>
        <StateChip kind={metrics.state} detail={metrics.detail} />
      </Row>
    );
  }
  return (
    <Row term="topology metrics" grade={metrics.value.current ? 'exact' : 'stale'} source="TOPOLOGY">
      <span className="td-value" data-cell="numeric">
        {metrics.value.measurements.length}
      </span>{' '}
      <span className="text-text-muted">
        measurements · {metrics.value.current ? 'current' : 'not current'} · watermark {metrics.value.watermark}
      </span>
    </Row>
  );
}
