import type { WorkProjectionSnapshotV1 } from '../../contracts/index.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import { Corners, Panel, Ticks, WorkspaceHeader } from '../../ui/instrument.tsx';
import { type DashboardScope, useScope } from '../../data/scope/store.ts';
import { WorkBoard, useSelectedTask } from './WorkBoard.tsx';
import { WorkCommands, WorkCreate } from './WorkCommands.tsx';
import { WorkTaskActivity } from './WorkTaskActivity.tsx';
import { resumeCursor, useWorkDelta, useWorkSnapshot } from './workQueries.ts';
import { useWorkAttempts } from './workViewsQueries.ts';
import { workAttemptReading, type WorkAttemptReading } from './workAttemptModel.ts';
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
import { WorkWorkloadView } from './views/WorkWorkloadView.tsx';

/**
 * Work — channel thirteen.
 *
 * This page reads. The daemon mounts the nine canonical Work routes and
 * contracts their payloads, so the projections below are the daemon's own
 * `WorkProjectionSnapshotV1` rather than an inferred stand-in. Every value
 * here came off a generated contract; nothing is inferred, and a route that
 * refuses is reported as the refusal it was. Execution belongs to the Workflow
 * runtime, which has its own workspace — this channel is the task graph.
 *
 * Five projections over ONE read. The switcher moves the camera and the
 * snapshot does not change underneath it, which is what makes the plan 11
 * mandate hold: a task selected in any projection stays selected in all of
 * them, because the selection lives in the address and no projection owns it.
 *
 * The four projections beside the board each encode at least one measurement
 * this build cannot take — effort, wall clock, observed order, executor
 * identity, concurrency. Those gaps are drawn inside each projection rather
 * than hidden behind an empty axis; `workViewsModel.ts` explains which belongs
 * to which and why.
 */

export function workScopeProvenance(scope: DashboardScope): string {
  switch (scope.kind) {
    case 'all':
      return 'canonical task graph · the active project · nine mounted routes';
    case 'project': {
      const identity = `${scope.label} (${scope.projectId})`;
      switch (scope.activation) {
        case 'active':
          return `canonical task graph · ${identity} · selected active project · nine mounted routes`;
        case 'selected':
          return `canonical task graph · ${identity} · selected project · nine mounted routes`;
        case 'unresolved':
          return `canonical task graph · ${identity} · selected project, registry unresolved · nine mounted routes`;
        case 'absent':
          return `canonical task graph · ${identity} · selected project absent from registry · nine mounted routes`;
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
  selected,
  onSelect,
}: {
  kind: WorkProjectionKind;
  snapshot: WorkProjectionSnapshotV1;
  attempts: WorkAttemptReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  switch (kind) {
    case 'board':
      return <WorkBoard snapshot={snapshot} selected={selected} onSelect={onSelect} />;
    case 'dag':
      return <WorkDagView snapshot={snapshot} selected={selected} onSelect={onSelect} />;
    case 'timeline':
      return (
        <WorkTimelineView
          snapshot={snapshot}
          attempts={attempts}
          selected={selected}
          onSelect={onSelect}
        />
      );
    case 'causal':
      return <WorkCausalView snapshot={snapshot} selected={selected} onSelect={onSelect} />;
    case 'workload':
      return <WorkWorkloadView snapshot={snapshot} selected={selected} onSelect={onSelect} />;
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
  const snapshot = useWorkSnapshot();
  // The execution record belongs to the timeline, so the attempt list is read
  // when that projection is the camera and not on every visit to the page.
  const attempts = useWorkAttempts(projection === 'timeline');
  const attemptReading = workAttemptReading(attempts.data);
  const result = snapshot.data;
  const value = result?.outcome === 'value' ? result.value : undefined;
  // Only asked for when the snapshot says it was capped or partial, so a
  // complete board issues no continuation request at all.
  const delta = useWorkDelta(value === undefined ? undefined : resumeCursor(value.coverage));

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
            {snapshot.isPending ? (
              <Panel legend="Work read model">
                <StateChip kind="loading" detail="reading the snapshot" />
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
                selected={selected}
                onSelect={setSelected}
              />
            )}
          </div>

          {value === undefined ? null : (
            <>
              {delta.data?.outcome === 'refused' ? (
                <Panel legend="Continuation">
                  <StateChip kind={delta.data.state} detail={delta.data.detail} />
                </Panel>
              ) : null}
              {delta.data?.outcome === 'value' ? (
                <Panel legend="Continuation">
                  <StateChip
                    kind="partial"
                    detail={`${delta.data.value.changed.length} changed, ${delta.data.value.removed.length} removed, through sequence ${delta.data.value.to_sequence}`}
                  />
                </Panel>
              ) : null}

              <div className="grid min-w-0 gap-3 lg:grid-cols-2">
                {selectedProjection === undefined ? (
                  <Panel legend="Commands">
                    <p className="text-2xs text-text-muted">
                      Select a task to see the commands its recorded state allows.
                    </p>
                  </Panel>
                ) : (
                  <WorkCommands projection={selectedProjection} snapshot={value} />
                )}
                <WorkCreate />
              </div>
            </>
          )}

        </div>
      </div>
    </div>
  );
}
