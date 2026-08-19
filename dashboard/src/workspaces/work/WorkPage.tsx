import type {
  ExecutionTopologyMetricsV1,
  ExecutionTopologyViewV1,
  WorkAttemptListV1,
} from '../../contracts/index.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import { Corners, Panel, Ticks, WorkspaceHeader } from '../../ui/instrument.tsx';
import { type DashboardScope, useScope } from '../../data/scope/store.ts';
import { WorkBoard, useSelectedTask } from './WorkBoard.tsx';
import { WorkCommands, WorkCreate } from './WorkCommands.tsx';
import { WorkEvidencePanel } from './WorkEvidencePanel.tsx';
import { WorkTaskActivity } from './WorkTaskActivity.tsx';
import {
  useWorkAttempts,
  useWorkGraphViews,
  useWorkTopology,
  useWorkTopologyMetrics,
} from './workViewsQueries.ts';
import { workAttemptReading, type WorkAttemptReading } from './workAttemptModel.ts';
import { workGraphReading, type WorkGraphReading } from './workGraphModel.ts';
import { WorkCausalView } from './views/WorkCausalView.tsx';
import { WorkDagView } from './views/WorkDagView.tsx';
import {
  PROJECTION_PANEL_ID,
  type WorkProjectionKind,
  WorkProjectionSwitcher,
  projectionNote,
  tabId,
  useWorkProjection,
} from './views/WorkProjectionSwitcher.tsx';
import { WorkTimelineView } from './views/WorkTimelineView.tsx';
import { WorkTopologyView } from './views/WorkTopologyView.tsx';
import { WorkWorkloadView } from './views/WorkWorkloadView.tsx';
import type { WorkResult } from './workApi.ts';
import { currentWorkProductView, type WorkProductView } from './workProductView.ts';

/**
 * Work — channel thirteen.
 *
 * This page reads one current `WorkGraphReadV1` and
 * reduces its exact product-graph entry to the local camera model; the legacy
 * projection snapshot is not a second authority. A route that refuses is
 * reported as the refusal it was. Execution belongs to the Workflow runtime,
 * which has its own workspace — this channel is the task graph.
 *
 * Six projections over ONE product graph version. The switcher moves the camera and the
 * graph does not change underneath it, which is what makes the plan 11
 * mandate hold: a task selected in any projection stays selected in all of
 * them, because the selection lives in the address and no projection owns it.
 *
 * Three reads feed the page: the product graph always, the attempt list under
 * the timeline and topology lens, and the canonical topology read under its
 * lens.
 * The graph read is what made effort, concurrency and churn measurable; wall
 * clock and observed execution order survive it as stated absences.
 * `workViewsModel.ts` explains which channel comes from which read and why the
 * two that are still absent cannot be filled from the ones that are not.
 */

export function workScopeProvenance(scope: DashboardScope): string {
  switch (scope.kind) {
    case 'all':
      return 'canonical task graph · the active project · exact product authority';
    case 'project': {
      const identity = `${scope.label} (${scope.projectId})`;
      switch (scope.activation) {
        case 'active':
          return `canonical task graph · ${identity} · selected active project · exact product authority`;
        case 'selected':
          return `canonical task graph · ${identity} · selected project · exact product authority`;
        case 'unresolved':
          return `canonical task graph · ${identity} · selected project, registry unresolved · exact product authority`;
        case 'absent':
          return `canonical task graph · ${identity} · selected project absent from registry · exact product authority`;
        default: {
          const exhaustive: never = scope.activation;
          return exhaustive;
        }
      }
    }
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}

/** The camera, applied. Exhaustive so a projection added to the switcher
 * cannot be left without something to draw. */
function WorkProjectionView({
  kind,
  snapshot,
  attempts,
  attemptList,
  topology,
  topologyMetrics,
  graph,
  selected,
  onSelect,
}: {
  kind: WorkProjectionKind;
  snapshot: WorkProductView;
  attempts: WorkAttemptReading;
  /** The raw attempt-list result, for the topology lens: its placement
   * derivations walk the attempts' execution envelopes, which the derived
   * reading deliberately does not restate. */
  attemptList: WorkResult<WorkAttemptListV1> | undefined;
  topology: WorkResult<ExecutionTopologyViewV1> | undefined;
  topologyMetrics: WorkResult<ExecutionTopologyMetricsV1> | undefined;
  graph: WorkGraphReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  switch (kind) {
    case 'board':
      return <WorkBoard snapshot={snapshot} graph={graph} selected={selected} onSelect={onSelect} />;
    case 'dag':
      return (
        <WorkDagView snapshot={snapshot} graph={graph} selected={selected} onSelect={onSelect} />
      );
    case 'timeline':
      return (
        <WorkTimelineView
          snapshot={snapshot}
          attempts={attempts}
          graph={graph}
          selected={selected}
          onSelect={onSelect}
        />
      );
    case 'causal':
      return (
        <WorkCausalView snapshot={snapshot} graph={graph} selected={selected} onSelect={onSelect} />
      );
    case 'workload':
      return (
        <WorkWorkloadView
          snapshot={snapshot}
          graph={graph}
          selected={selected}
          onSelect={onSelect}
        />
      );
    case 'topology':
      return (
        <WorkTopologyView
          snapshot={snapshot}
          attemptList={attemptList}
          topology={topology}
          metrics={topologyMetrics}
          graph={graph}
          selected={selected}
          onSelect={onSelect}
        />
      );
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

export function WorkPage() {
  const scope = useScope((state) => state.scope);
  const [selected, setSelected] = useSelectedTask();
  const [projection, setProjection] = useWorkProjection();
  // The execution record belongs to the timeline and the topology lens, so
  // the attempt list is read when one of those projections is the camera and
  // not on every visit to the page.
  const attempts = useWorkAttempts(projection === 'timeline' || projection === 'topology');
  const topology = useWorkTopology(projection === 'topology');
  // The accounting read behind the topology lens's integration and stack
  // cards; issued only when that lens is the camera.
  const topologyMetrics = useWorkTopologyMetrics(projection === 'topology');
  const attemptReading = workAttemptReading(attempts.data);
  // The graph hook bootstraps against profile ownership, then re-reads against
  // the exact repository scope returned in the daemon's response envelope.
  const graph = useWorkGraphViews(true);
  const graphReading = workGraphReading(graph.data);
  const result = currentWorkProductView(graph.data);
  const value = result?.outcome === 'value' ? result.value : undefined;

  const selectedProjection = value?.projections.find(
    (projection) => projection.task_id === selected,
  );

  return (
    <div
      className="min-w-0"
      data-work-authority={value === undefined ? 'unread' : 'read'}
      data-testid="work-page"
    >
      <WorkspaceHeader
        path="work"
        title="Work"
        note={workScopeProvenance(scope)}
      />

      <div
        role="region"
        aria-label="Work content"
        tabIndex={0}
        className="relative min-w-0 overflow-x-auto p-3"
      >
        <Corners />
        <Ticks />

        <div className="flex min-w-0 flex-col gap-3">
          {/* The live stream sits in the body rather than in the header's
            * actions. `WorkspaceHeader` is a fixed `h-9` row, and this chip
            * carries a sentence — "subscribed · connecting" and its longer
            * siblings — which wraps to two and three lines below `md` and at
            * 400% zoom, rendering outside the header box. It also reads better
            * here: it is supplementary evidence about a stream, not the state
            * of the page. */}
          <div className="flex flex-wrap items-center gap-2">
            <WorkTaskActivity kind="partial" />
          </div>

          {/* The camera sits above every state below it, including the
            * refusals: which projection you are looking at is a property of
            * the page, not of whether the read succeeded, and losing the
            * switcher on a 503 would strand a reader in a projection they
            * cannot leave. */}
          <div className="flex min-w-0 flex-col gap-1.5">
            <WorkProjectionSwitcher active={projection} onSelect={setProjection} />
            <p className="text-3xs text-text-muted">{projectionNote(projection)}</p>
          </div>

          {/* The region the camera points at, drawn in every state rather than
            * only when there is a projection to put in it. The tabs above
            * declare that they control this region, so it has to exist for as
            * long as they do — and under a refusal it is where a reader who
            * just moved the camera looks to find out why nothing moved. */}
          <div
            role="tabpanel"
            id={PROJECTION_PANEL_ID}
            aria-labelledby={tabId(projection)}
            className="flex min-w-0 flex-col gap-3"
          >
            {graph.isPending ? (
              <Panel legend="Work read model">
                <StateChip kind="loading" detail="reading the product graph" />
              </Panel>
            ) : null}

            {result?.outcome === 'refused' ? (
              <Panel legend="Work read model">
                {/* The daemon's own reason, in the taxonomy's vocabulary. An
                  * unavailable runtime and an empty board are different things and
                  * must never render alike. */}
                <StateChip kind={result.state} detail={result.detail} />
                <p className="mt-1 text-3xs text-text-muted">
                  No board is drawn. This build reads the Work routes and does not
                  infer their contents when they refuse.
                </p>
              </Panel>
            ) : null}

            {value === undefined ? null : (
              <WorkProjectionView
                kind={projection}
                snapshot={value}
                attempts={attemptReading}
                attemptList={attempts.data}
                topology={topology.data}
                topologyMetrics={topologyMetrics.data}
                graph={graphReading}
                selected={selected}
                onSelect={setSelected}
              />
            )}
          </div>

          {value === undefined ? null : (
            <>
              <div className="grid min-w-0 gap-3 lg:grid-cols-2">
                {selectedProjection === undefined ? (
                  <Panel legend="Commands">
                    <p className="text-2xs text-text-muted">
                      Select a task to see the commands its recorded state allows.
                    </p>
                  </Panel>
                ) : (
                  <div className="grid min-w-0 gap-3">
                    <WorkCommands
                      projection={selectedProjection}
                      graph={graph.data}
                    />
                    <WorkEvidencePanel taskId={selectedProjection.task_id} graph={graph.data} />
                  </div>
                )}
                <WorkCreate graph={graph.data} />
              </div>
            </>
          )}

        </div>
      </div>
    </div>
  );
}
