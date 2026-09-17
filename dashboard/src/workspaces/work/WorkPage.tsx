import { useMemo, type ReactNode } from 'react';
import type {
  ExecutionTopologyMetricsV1,
  ExecutionTopologyViewV1,
  FeedbackProximityReadResultV1,
  WorkAttemptListV1,
} from '../../contracts/index.ts';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { Corners, Panel, Ticks, WorkspaceHeader } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import { type DashboardScope, useScope } from '../../data/scope/store.ts';
import { WorkBoard, useSelectedTask } from './WorkBoard.tsx';
import { WorkCreate } from './WorkCommands.tsx';
import { WorkInspector } from './WorkInspector.tsx';
import { WorkActivityLedger } from './WorkTaskActivity.tsx';
import {
  useWorkAttempts,
  useWorkGraphViews,
  useWorkTopology,
  useWorkTopologyMetrics,
} from './workViewsQueries.ts';
import { workAttemptReading, type WorkAttemptReading } from './workAttemptModel.ts';
import {
  workGraphReading,
  type WorkGraphReading,
} from './workGraphModel.ts';
import type { ConcurrentAttemptsReading } from './workConcurrentAttempts.ts';
import { useWorkConcurrentAttempts } from './workConcurrentAttemptsQuery.ts';
import { WorkConcurrentAttemptsView } from './views/WorkConcurrentAttemptsView.tsx';
import { WorkCausalView } from './views/WorkCausalView.tsx';
import { WorkDagView } from './views/WorkDagView.tsx';
import {
  PROJECTION_PANEL_ID,
  type WorkProjectionKind,
  WorkProjectionSwitcher,
  projectionLabel,
  projectionNote,
  tabId,
  useWorkProjection,
} from './views/WorkProjectionSwitcher.tsx';
import { WorkTimelineView } from './views/WorkTimelineView.tsx';
import { WorkTopologyView } from './views/WorkTopologyView.tsx';
import { WorkWorkloadView } from './views/WorkWorkloadView.tsx';
import type { WorkResult } from './workApi.ts';
import { currentWorkProductView, type WorkProductView } from './workProductView.ts';
import { workDagReading } from './workViewsModel.ts';

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
 * graph does not change underneath it: a task selected in any projection
 * stays selected in all of
 * them, because the selection lives in the address and no projection owns it.
 *
 * The page is composed as the V2 shell names its regions: the main aperture
 * holds the camera, the projection it points at, and the live task-activity
 * ledger; the workspace-owned inspector opens beside it on selection and
 * reflows underneath below `xl`; and a Work register strip at the foot
 * separates the graph read, the immutable version, the selection, and the
 * camera so none of them can stand in for another.
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
  concurrent,
  proximity,
  selectedEncounterId,
  graph,
  selected,
  onSelect,
  onSelectEncounter,
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
  concurrent: ConcurrentAttemptsReading;
  proximity: WorkResult<FeedbackProximityReadResultV1> | undefined;
  selectedEncounterId: string | null;
  graph: WorkGraphReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
  onSelectEncounter: (encounterId: string | null) => void;
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
    case 'concurrent-attempts':
      return (
        <WorkConcurrentAttemptsView
          reading={concurrent}
          proximity={proximity}
          selectedEncounterId={selectedEncounterId}
          onSelectEncounter={onSelectEncounter}
        />
      );
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** One numbered register of the Work strip: segment number, engraved label,
 * a swatch that never carries the state alone, and the reading. */
function RegisterCell({
  code,
  label,
  tone,
  children,
}: {
  code: string;
  label: string;
  tone: string | null;
  children: ReactNode;
}) {
  return (
    <div className="flex min-w-0 items-center gap-2 border-r border-edge-subtle px-3 py-1.5 last:border-r-0">
      <span aria-hidden className="td-value text-3xs text-text-muted" data-cell="numeric">
        {code}
      </span>
      <span className="td-legend">{label}</span>
      {tone === null ? (
        <span aria-hidden className="size-2 shrink-0 border border-dashed border-text-muted" />
      ) : (
        <span aria-hidden className={cn('size-2 shrink-0', tone)} />
      )}
      <span className="td-value min-w-0 truncate text-2xs">{children}</span>
    </div>
  );
}

function graphRegister(
  pending: boolean,
  result: WorkResult<WorkProductView> | undefined,
): { tone: string | null; text: string; state: DomainStateKind } {
  if (pending) return { tone: 'bg-state-loading', text: 'reading', state: 'loading' };
  if (result === undefined) return { tone: null, text: 'unread', state: 'unknown' };
  if (result.outcome === 'refused') {
    return { tone: 'bg-state-error', text: result.state.replaceAll('_', ' '), state: result.state };
  }
  return {
    tone: result.value.projections.length === 0 ? 'bg-state-complete-zero' : 'bg-state-ready',
    text: result.value.projections.length === 0 ? 'served · empty' : 'ready',
    state: result.value.projections.length === 0 ? 'complete_zero_findings' : 'ready',
  };
}

/**
 * The Work register: four independent facts about this page, in the same
 * grammar as the shell's status strip. The graph read, the immutable version,
 * the selection, and the camera are separate facts — a served graph does not
 * imply a selection, and a selection does not imply a fresh read.
 */
function WorkRegister({
  pending,
  result,
  selected,
  projection,
}: {
  pending: boolean;
  result: WorkResult<WorkProductView> | undefined;
  selected: string | null;
  projection: WorkProjectionKind;
}) {
  const graph = graphRegister(pending, result);
  const value = result?.outcome === 'value' ? result.value : undefined;
  return (
    <div
      role="group"
      aria-label="Work register"
      className="flex min-w-0 flex-wrap items-stretch border border-edge-subtle bg-surface-1"
      data-work-register={graph.state}
    >
      <RegisterCell code="01" label="Work graph" tone={graph.tone}>
        {graph.text}
      </RegisterCell>
      <RegisterCell code="02" label="Version" tone={value === undefined ? null : 'bg-accent'}>
        {value === undefined ? 'unread' : `v${value.graph_version} · seq ${value.event_sequence} · immutable`}
      </RegisterCell>
      <RegisterCell
        code="03"
        label="Selection"
        tone={selected === null ? null : value?.projections.some((task) => task.task_id === selected) ? 'bg-accent' : 'bg-state-partial'}
      >
        {selected === null
          ? 'none'
          : value?.projections.some((task) => task.task_id === selected)
            ? `one task · ${selected}`
            : `${selected} · not in this graph version`}
      </RegisterCell>
      <RegisterCell code="04" label="Camera" tone="bg-edge-strong">
        {projectionLabel(projection).toLowerCase()}
      </RegisterCell>
    </div>
  );
}

export function WorkPage() {
  const scope = useScope((state) => state.scope);
  const [selected, setSelected] = useSelectedTask();
  const [projection, setProjection] = useWorkProjection();
  // The execution record belongs to the timeline and the topology lens, so
  // the attempt list is read when one of those projections is the camera and
  // not on every visit to the page.
  const attempts = useWorkAttempts(
    projection === 'timeline' ||
      projection === 'topology' ||
      projection === 'concurrent-attempts',
  );
  const topology = useWorkTopology(projection === 'topology');
  // The accounting read behind the topology lens's integration and stack
  // cards; issued only when that lens is the camera.
  const topologyMetrics = useWorkTopologyMetrics(projection === 'topology');
  const attemptReading = useMemo(() => workAttemptReading(attempts.data), [attempts.data]);
  // The graph hook bootstraps against profile ownership, then re-reads against
  // the exact repository scope returned in the daemon's response envelope.
  const graph = useWorkGraphViews(true);
  const graphReading = useMemo(() => workGraphReading(graph.data), [graph.data]);
  const concurrent = useWorkConcurrentAttempts(
    graph.data,
    graphReading,
    selected,
    projection,
  );
  const result = useMemo(() => currentWorkProductView(graph.data), [graph.data]);
  const value = result?.outcome === 'value' ? result.value : undefined;
  const dag = useMemo(
    () => workDagReading(value?.projections ?? [], graphReading),
    [value?.projections, graphReading],
  );

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
        actions={
          <span className="td-title shrink-0 text-text-muted" data-work-header-camera={concurrent.active}>
            / {projectionLabel(concurrent.active)}
          </span>
        }
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
          {/* The aperture and the inspector share one grid. Below `xl` the
            * inspector reflows under the aperture rather than squeezing it:
            * at 200% zoom on a 1440px display the CSS viewport is 720px, and
            * a 22rem column beside a graph would leave neither readable. */}
          <div className="grid min-w-0 gap-3 xl:grid-cols-[minmax(0,1fr)_minmax(18rem,22rem)]">
            <div className="flex min-w-0 flex-col gap-3" data-work-aperture>
              {/* The camera sits above every state below it, including the
                * refusals: which projection you are looking at is a property of
                * the page, not of whether the read succeeded, and losing the
                * switcher on a 503 would strand a reader in a projection they
                * cannot leave. */}
              <div className="flex min-w-0 flex-col gap-1.5">
                <WorkProjectionSwitcher
                  active={concurrent.active}
                  onSelect={setProjection}
                  projections={concurrent.projections}
                />
                <p className="text-3xs text-text-muted">{projectionNote(concurrent.active)}</p>
              </div>

              {/* The region the camera points at, drawn in every state rather than
                * only when there is a projection to put in it. The tabs above
                * declare that they control this region, so it has to exist for as
                * long as they do — and under a refusal it is where a reader who
                * just moved the camera looks to find out why nothing moved. */}
              <div
                role="tabpanel"
                id={PROJECTION_PANEL_ID}
                aria-labelledby={tabId(concurrent.active)}
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
                    kind={concurrent.active}
                    snapshot={value}
                    attempts={attemptReading}
                    attemptList={attempts.data}
                    topology={topology.data}
                    topologyMetrics={topologyMetrics.data}
                    concurrent={concurrent.reading}
                    proximity={concurrent.proximity}
                    selectedEncounterId={concurrent.selectedEncounterId}
                    graph={graphReading}
                    selected={selected}
                    onSelect={setSelected}
                    onSelectEncounter={concurrent.selectEncounter}
                  />
                )}
              </div>

              <WorkActivityLedger />
            </div>

            <div className="flex min-w-0 flex-col gap-3" data-work-inspector-column>
              {value === undefined ? (
                <Panel legend="Selected task">
                  {graph.isPending ? (
                    <StateChip kind="loading" detail="the inspector opens once the graph read answers" />
                  ) : (
                    <p className="text-2xs text-text-muted">
                      No graph version is served, so there is no task to inspect. The refusal
                      above is the reason.
                    </p>
                  )}
                </Panel>
              ) : (
                <>
                  <WorkInspector
                    task={selectedProjection ?? null}
                    snapshot={value}
                    graph={graph.data}
                    graphReading={graphReading}
                    dag={dag}
                    topology={topology.data}
                    topologyMetrics={topologyMetrics.data}
                    onSelect={setSelected}
                  />
                  <details className="min-w-0 border border-edge-subtle bg-surface-1" data-work-create>
                    <summary className="flex min-h-[44px] cursor-pointer items-center gap-2 px-2.5 text-2xs text-text-secondary hover:bg-surface-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent">
                      <span className="td-legend">create work</span>
                      <span aria-hidden className="td-rule" />
                      <span className="text-3xs text-text-muted">prepare a task against the current graph</span>
                    </summary>
                    <div className="border-t border-edge-subtle">
                      <WorkCreate graph={graph.data} />
                    </div>
                  </details>
                </>
              )}
            </div>
          </div>

          <WorkRegister
            pending={graph.isPending}
            result={result}
            selected={selected}
            projection={concurrent.active}
          />
        </div>
      </div>
    </div>
  );
}
