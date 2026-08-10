import type { WorkProjection, WorkProjectionSnapshotV1 } from '../../../contracts/index.ts';
import { StateChip } from '../../../ui/StateChip.tsx';
import { MeterRow, Panel } from '../../../ui/instrument.tsx';
import { cn } from '../../../ui/cn.ts';
import { relativeAge } from '../../../ui/time.ts';
import { kindColorVars } from '../../../viz/graph/kindColor.ts';
import { coverageReading } from '../workModel.ts';
import {
  graphRuntimeAttempts,
  terminalWorkAttempt,
  type WorkGraphReading,
} from '../workGraphModel.ts';
import {
  type WorkloadReading,
  type WorkloadRegion,
  workloadReading,
} from '../workViewsModel.ts';
import { ChannelAbsence, ChannelLedger, EmptyReading, ViewCaption } from './WorkViewChannel.tsx';

/**
 * Workload / executor / model — the cortex aggregation over runs.
 *
 * A cortex draws regions whose area is mass, whose contours are concurrency and
 * whose heat is recent churn. This build now has all three — the work-product
 * graph read carries declared effort, both concurrency figures and the per-task
 * change instants — and it still does not draw them ONTO the regions, which is
 * the decision worth stating.
 *
 * The regions are the exact graph's run/task attempt incidence, so a region's length stays
 * TASK COUNT: captioned as that, printed as that beside every bar, never called
 * mass, cost or load. Effort, concurrency and churn are properties of the whole
 * work-product graph read at one version, over a task set that need not be the
 * task set this snapshot page returned. Rescaling a run's bar by declared graph
 * effort would draw a figure across that seam that neither read holds, so the
 * measurements are printed as figures of their own, beside the aggregation and
 * captioned with the read they came from.
 *
 * No contour is drawn and no heat ramp appears anywhere on this page. The hue
 * that separates one region from its neighbour is the console's categorical
 * kind arc; a warm-to-cool ramp standing in for churn would be read as a
 * measurement of the regions it was painted on, which is precisely the seam
 * above.
 *
 * A run is not an executor. `WorkProjection` names no provider, model or
 * agent, so a region is labelled by its run id and says the executor behind it
 * is unnamed. Tasks the store attaches to no run keep their own band and are
 * drawn hollow — folding them into a region would invent the attribution the
 * store declined to make.
 *
 * Accessibility. The aggregation is a ranked list of lists of buttons, so the
 * visualization IS the accessible structure: Tab walks the regions in rank
 * order and every task is a real control. The rail on each region restates a
 * figure printed beside it and stays out of the accessibility tree.
 */

/** One task inside one region, with the attempts that run made for it. */
interface RegionMember {
  readonly taskId: string;
  readonly title: string;
  readonly attemptCount: number;
  readonly terminal: boolean;
}

/**
 * Which tasks each region holds.
 *
 * `WorkloadRegion` carries counts rather than members, and membership is
 * exactly the incidence those counts were taken over: a task falls in a run's
 * region when the exact graph attributes an attempt to that run. One task can fall in
 * several regions, so the region counts are a reading per run and not a
 * partition of the board — which is why the bars are ranked against the
 * largest region rather than drawn as shares of a whole.
 */
function regionMembers(
  projections: readonly WorkProjection[],
  graph: WorkGraphReading,
): ReadonlyMap<string, readonly RegionMember[]> {
  const members = new Map<string, RegionMember[]>();
  const titles = new Map(projections.map((projection) => [projection.task_id, projection.title]));
  const perTask = new Map<string, Map<string, { attemptCount: number; terminal: boolean }>>();
  for (const attempt of graphRuntimeAttempts(graph)) {
    const perRun = perTask.get(attempt.identity.task_id) ?? new Map();
    perTask.set(attempt.identity.task_id, perRun);
    const tally = perRun.get(attempt.identity.run_id);
    perRun.set(attempt.identity.run_id, {
      attemptCount: (tally?.attemptCount ?? 0) + 1,
      terminal: (tally?.terminal ?? false) || terminalWorkAttempt(attempt.state),
    });
  }
  for (const [taskId, perRun] of perTask) {
    for (const [runId, tally] of perRun) {
      const member: RegionMember = {
        taskId,
        title: titles.get(taskId) ?? taskId,
        attemptCount: tally.attemptCount,
        terminal: tally.terminal,
      };
      const bucket = members.get(runId);
      if (bucket === undefined) members.set(runId, [member]);
      else bucket.push(member);
    }
  }
  for (const bucket of members.values()) {
    bucket.sort((a, b) => b.attemptCount - a.attemptCount || a.taskId.localeCompare(b.taskId));
  }
  return members;
}

export function WorkWorkloadView({
  snapshot,
  graph,
  selected,
  onSelect,
}: {
  snapshot: WorkProjectionSnapshotV1;
  graph: WorkGraphReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const reading = workloadReading(snapshot.projections, graph);
  const coverage = coverageReading(snapshot.coverage);
  const members = regionMembers(snapshot.projections, graph);
  const attributed = reading.taskCount - reading.unattributed.length;
  const runtime = reading.runtime;

  return (
    <div className="flex min-w-0 flex-col gap-3" data-work-view="workload">
      <Panel
        legend="Runs as regions"
        actions={
          <div className="flex flex-wrap gap-1">
            <StateChip kind={coverage.state} detail={coverage.detail} />
            {runtime.available ? (
              <StateChip
                kind={runtime.value.complete ? 'ready' : 'partial'}
                detail={runtime.value.complete ? 'attempt coverage complete' : 'attempt counts are floors'}
              />
            ) : (
              <StateChip kind={runtime.state} detail="attempt attribution unavailable" />
            )}
          </div>
        }
        elevation="well"
      >
        {!runtime.available ? (
          <ChannelAbsence measure="run and task attempt attribution" channel={runtime} />
        ) : (
          <div className="flex min-w-0 flex-col gap-3">
          {/* The aggregation ratio leads the panel, because a cortex that does
            * not print how many things it folded into how few is a picture of
            * a number nobody can recover. */}
          <ViewCaption
            population={`${reading.taskCount} tasks ⟵ ${reading.regions.length} regions`}
            note={`${runtime.value.complete ? '' : 'at least '}${attributed} of ${reading.taskCount} in a region · ${reading.attemptCount} returned attempts`}
          >
            <span data-work-aggregation={`${reading.taskCount}:${reading.regions.length}`}>
              region length is task count, not effort
            </span>
          </ViewCaption>

          <Aggregation
            reading={reading}
            members={members}
            selected={selected}
            onSelect={onSelect}
          />
          </div>
        )}
      </Panel>

      <div className="grid min-w-0 gap-3 lg:grid-cols-2">
        <EffortMass reading={reading} />
        <Concurrency reading={reading} />
      </div>

      <div className="grid min-w-0 gap-3 lg:grid-cols-2">
        <Churn reading={reading} />
        <RuntimeAttempts reading={reading} />
      </div>

      <div className="grid min-w-0 gap-3 lg:grid-cols-2">
        <Unattributed reading={reading} selected={selected} onSelect={onSelect} />
        <ChannelLedger
          legend="Measurements this projection could not take"
          channels={[
            { measure: 'task mass by effort', channel: reading.effortMass },
            { measure: 'concurrency contours', channel: reading.concurrency },
            { measure: 'recent churn', channel: reading.churn },
            { measure: 'live attempt state', channel: reading.runtime },
          ]}
        />
      </div>
    </div>
  );
}

/**
 * Declared effort mass, and the runtime split of it.
 *
 * Two readings in one panel because they fail apart: the total is declared by
 * the graph and always arrives with it, while ready/running/blocked are counted
 * against live attempt state and the authority withholds all three unless the
 * runtime projection covered every attempt. A page that printed zeros for the
 * split under incomplete coverage would report an idle graph; the split is
 * drawn as its own absence instead.
 */
function EffortMass({ reading }: { reading: WorkloadReading }) {
  const mass = reading.effortMass;
  return (
    <Panel
      legend="Declared effort mass"
      actions={
        mass.available ? (
          <StateChip kind="ready" detail={`${mass.value.total} total`} />
        ) : (
          <StateChip kind={mass.state} detail="not read" />
        )
      }
    >
      {!mass.available ? (
        <p className="text-3xs leading-snug text-text-muted">{mass.detail}</p>
      ) : (
        <div className="flex min-w-0 flex-col gap-2" data-work-effort={mass.value.total}>
          <p className="text-3xs leading-snug text-text-muted">
            {mass.value.total} declared effort across the whole work-product graph version. This
            is not the mass of the regions above — those are the tasks this snapshot page
            returned, and the two reads need not cover the same set.
          </p>
          {mass.value.split.available ? (
            <ul
              className="flex min-w-0 flex-col gap-1"
              data-work-effort-split={`${mass.value.split.value.ready}/${mass.value.split.value.running}/${mass.value.split.value.blocked}`}
            >
              {(
                [
                  ['Ready', mass.value.split.value.ready],
                  ['Running', mass.value.split.value.running],
                  ['Blocked', mass.value.split.value.blocked],
                ] as const
              ).map(([label, value]) => (
                <li key={label} className="min-w-0">
                  <MeterRow
                    label={label}
                    title={label}
                    value={value}
                    fraction={mass.value.total === 0 ? null : value / mass.value.total}
                  />
                </li>
              ))}
            </ul>
          ) : (
            <ChannelAbsence
              measure="ready, running and blocked effort"
              channel={mass.value.split}
            />
          )}
        </div>
      )}
    </Panel>
  );
}

/** What was asked for against what is running. Both figures or neither: the
 * authority counts them against the same runtime coverage. */
function Concurrency({ reading }: { reading: WorkloadReading }) {
  const concurrency = reading.concurrency;
  return (
    <Panel
      legend="Concurrency"
      actions={
        concurrency.available ? (
          <StateChip
            kind={
              concurrency.value.actual > concurrency.value.requested ? 'conflicting' : 'ready'
            }
            detail={`${concurrency.value.actual} of ${concurrency.value.requested}`}
          />
        ) : (
          <StateChip kind={concurrency.state} detail="not counted" />
        )
      }
    >
      {!concurrency.available ? (
        <p className="text-3xs leading-snug text-text-muted">{concurrency.detail}</p>
      ) : (
        <div
          className="flex min-w-0 flex-col gap-2"
          data-work-concurrency={`${concurrency.value.actual}/${concurrency.value.requested}`}
        >
          <MeterRow
            label="Requested"
            title="Requested concurrency"
            value={concurrency.value.requested}
            fraction={null}
          />
          <MeterRow
            label="Actual"
            title="Actual concurrency"
            value={concurrency.value.actual}
            fraction={
              concurrency.value.requested === 0
                ? null
                : concurrency.value.actual / concurrency.value.requested
            }
          />
          {concurrency.value.actual > concurrency.value.requested ? (
            <p className="text-3xs leading-snug text-text-muted">
              More is running than was asked for. Both figures come off one graph version and
              disagreeing is the reading — neither is corrected against the other.
            </p>
          ) : null}
        </div>
      )}
    </Panel>
  );
}

/**
 * Recent change, against the instant the graph version was observed at.
 *
 * The window is a rendering parameter and is printed as one; the measurement is
 * each task's real distance from the observation instant. An update recorded
 * LATER than that instant is counted apart rather than clamped into the window:
 * the two clocks disagree, and that is a reading about the read.
 */
function Churn({ reading }: { reading: WorkloadReading }) {
  const churn = reading.churn;
  return (
    <Panel
      legend="Recent change"
      actions={
        churn.available ? (
          <StateChip
            kind={churn.value.recent.length === 0 ? 'complete_zero_findings' : 'ready'}
            detail={`${churn.value.recent.length} of ${churn.value.counted}`}
          />
        ) : (
          <StateChip kind={churn.state} detail="not measured" />
        )
      }
    >
      {!churn.available ? (
        <p className="text-3xs leading-snug text-text-muted">{churn.detail}</p>
      ) : (
        <div
          className="flex min-w-0 flex-col gap-2"
          data-work-churn={churn.value.recent.length}
          data-work-churn-counted={churn.value.counted}
        >
          <p className="text-3xs leading-snug text-text-muted">
            {churn.value.recent.length} of {churn.value.counted}{' '}
            {churn.value.counted === 1 ? 'task' : 'tasks'} changed within{' '}
            {Math.round(churn.value.window / 3_600_000_000)} hours of the instant this graph
            version was observed at. The window is a choice about what to list; the age beside
            each task is the measurement.
            {churn.value.ahead > 0
              ? ` ${churn.value.ahead} ${churn.value.ahead === 1 ? 'task records a change' : 'tasks record a change'} later than that instant, counted here and listed nowhere: the two clocks disagree and neither is corrected.`
              : ''}
          </p>
          {churn.value.recent.length === 0 ? (
            <EmptyReading>
              No task changed inside the window. The graph was read and its tasks were compared
              against the instant of the read — this is a measured quiet, not an unmeasured one.
            </EmptyReading>
          ) : (
            <ul className="flex min-w-0 flex-col gap-1">
              {churn.value.recent.map((entry) => (
                <li
                  key={entry.taskId}
                  className="flex min-w-0 items-center gap-2 text-2xs"
                  data-work-churn-task={entry.taskId}
                >
                  <span aria-hidden className="size-1.5 shrink-0 bg-state-ready" />
                  <span className="min-w-0 flex-1 truncate font-mono text-3xs text-text-secondary">
                    {entry.taskId}
                  </span>
                  <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
                    {relativeAge(entry.updatedAt / 1_000_000, churn.value.observedAt / 1_000_000) ??
                      'age unread'}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </Panel>
  );
}

/**
 * The live attempt projection under this graph version.
 *
 * The distinction this panel exists to hold: `unavailable` coverage means the
 * attempts could not be measured, and an empty list under `complete` coverage
 * means nothing is running. The first is an absence with the read's reason on
 * it; the second is a reading a person can act on. They are never drawn alike.
 */
function RuntimeAttempts({ reading }: { reading: WorkloadReading }) {
  const runtime = reading.runtime;
  return (
    <Panel
      legend="Live attempts"
      actions={
        runtime.available ? (
          <StateChip
            kind={
              !runtime.value.complete
                ? 'partial'
                : runtime.value.attempts.length === 0
                  ? 'complete_zero_findings'
                  : 'ready'
            }
            detail={`${runtime.value.attempts.length}`}
          />
        ) : (
          <StateChip kind={runtime.state} detail="unmeasured" />
        )
      }
    >
      {!runtime.available ? (
        <p className="text-3xs leading-snug text-text-muted" data-work-runtime="unavailable">
          {runtime.detail}
        </p>
      ) : (
        <div
          className="flex min-w-0 flex-col gap-2"
          data-work-runtime={runtime.value.complete ? 'complete' : 'partial'}
          data-work-runtime-attempts={runtime.value.attempts.length}
        >
          {runtime.value.complete ? null : (
            <p className="text-3xs leading-snug text-text-muted">
              The projection reached {runtime.value.unavailable}{' '}
              {runtime.value.unavailable === 1 ? 'attempt' : 'attempts'} it could not read, so
              every count here is a floor.
            </p>
          )}
          {runtime.value.attempts.length === 0 ? (
            <EmptyReading>
              {runtime.value.complete
                ? 'The runtime projection covered every attempt under this graph version and found none in flight. Nothing is running, and that is a reading rather than a gap.'
                : 'No attempt this projection could read is in flight, and it could not read all of them, so this is a floor of zero rather than a measurement of none.'}
            </EmptyReading>
          ) : (
            <ul className="flex min-w-0 flex-col gap-1">
              {runtime.value.attempts.map((attempt) => (
                <li
                  key={attempt.attemptId}
                  className="flex min-w-0 items-center gap-2 text-2xs"
                  data-work-runtime-attempt={attempt.attemptId}
                  data-work-runtime-state={attempt.state}
                >
                  <span className="min-w-0 flex-1 truncate font-mono text-3xs text-text-secondary">
                    {attempt.taskId} · {attempt.runId}
                  </span>
                  <span className="shrink-0 text-3xs text-text-muted">{attempt.state}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </Panel>
  );
}

/** The three readings this panel can have: an empty board, a board no run has
 * touched, and an aggregation. The middle one is stated rather than drawn. */
function Aggregation({
  reading,
  members,
  selected,
  onSelect,
}: {
  reading: WorkloadReading;
  members: ReadonlyMap<string, readonly RegionMember[]>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  if (reading.taskCount === 0) {
    return (
      <EmptyReading>
        The snapshot returned no tasks, so there is nothing to aggregate. This is the daemon
        reporting an empty board, not an aggregation that failed to draw.
      </EmptyReading>
    );
  }
  if (reading.regions.length === 0) {
    return (
      <EmptyReading>
        The exact graph attributes no attempt to any task the snapshot returned, so no region exists
        to aggregate into and every task sits in the unattributed band below. A degenerate
        distribution is said rather than drawn.
      </EmptyReading>
    );
  }
  // Ranked against the largest region rather than against the board: a task
  // can fall in several regions, so the counts have no denominator that sums
  // to one and a share of the whole would be a number this view invented.
  const widest = reading.regions.reduce((most, region) => Math.max(most, region.taskCount), 0);
  return (
    <ol className="flex min-w-0 flex-col gap-2" data-work-regions={reading.regions.length}>
      {reading.regions.map((region) => (
        <li key={region.runId} className="min-w-0">
          <Region
            region={region}
            members={members.get(region.runId) ?? []}
            fraction={widest === 0 ? null : region.taskCount / widest}
            selected={selected}
            onSelect={onSelect}
          />
        </li>
      ))}
    </ol>
  );
}

function Region({
  region,
  members,
  fraction,
  selected,
  onSelect,
}: {
  region: WorkloadRegion;
  members: readonly RegionMember[];
  fraction: number | null;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  return (
    <div
      className="flex min-w-0 flex-col gap-1.5 border border-edge-subtle bg-surface-1 p-2"
      data-work-region={region.runId}
      data-work-region-tasks={region.taskCount}
    >
      <MeterRow
        leading={
          // Hue tells one region from the next and claims nothing else. The arc
          // is categorical and stable per run id, and the run id is printed
          // beside it because it is the only identity this read carries.
          <span
            aria-hidden
            style={kindColorVars(region.runId)}
            className="size-2 shrink-0 bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
          />
        }
        label={<span className="truncate font-mono text-2xs">{region.runId}</span>}
        title={region.runId}
        value={region.taskCount}
        fraction={fraction}
      />
      <p className="text-3xs leading-snug text-text-muted">
        {region.taskCount} {region.taskCount === 1 ? 'task' : 'tasks'} · {region.attemptCount}{' '}
        attempts · {region.terminalCount} terminal · executor unnamed
      </p>
      <ul className="flex min-w-0 flex-wrap gap-1.5">
        {members.map((member) => (
          <li key={member.taskId} className="min-w-0">
            <TaskMark
              member={member}
              selected={selected === member.taskId}
              onSelect={onSelect}
            />
          </li>
        ))}
      </ul>
    </div>
  );
}

function TaskMark({
  member,
  selected,
  onSelect,
}: {
  member: RegionMember;
  selected: boolean;
  onSelect: (taskId: string) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => onSelect(member.taskId)}
      aria-pressed={selected}
      // 44px explicitly rather than a spacing utility: this app's root font
      // size is 14px, so `min-h-11` computes to 38.5px and lands under the
      // target size the accessibility gate measures.
      className={cn(
        'flex min-h-[44px] min-w-0 max-w-[16rem] flex-col justify-center gap-0.5 border px-2 py-1 text-left',
        'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent',
        selected
          ? 'border-accent bg-surface-3'
          : 'border-edge-subtle bg-surface-2 hover:bg-surface-3',
      )}
      data-work-task={member.taskId}
      data-work-attempts={member.attemptCount}
    >
      <span className="min-w-0 truncate text-2xs text-text-primary">{member.title}</span>
      <span className="truncate text-3xs text-text-muted">
        {member.attemptCount} attempts in this region
        {member.terminal ? ' · terminal' : ''}
      </span>
    </button>
  );
}

/**
 * Work no run claims.
 *
 * The exact graph attributes no runtime attempt to these tasks, so no region can hold them
 * without the drawing choosing an executor for them. They keep their own band
 * and are drawn hollow — outlined, unfilled — which is what "the executor the
 * store cannot name is not guessed" looks like on the page.
 */
function Unattributed({
  reading,
  selected,
  onSelect,
}: {
  reading: WorkloadReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const runtime = reading.runtime;
  if (!runtime.available) {
    return (
      <Panel
        legend="Unattributed work"
        actions={<StateChip kind={runtime.state} detail="attempt attribution unavailable" />}
      >
        <ChannelAbsence measure="tasks with no attributed attempt" channel={runtime} />
      </Panel>
    );
  }
  if (!runtime.value.complete) {
    return (
      <Panel
        legend="Unattributed work"
        actions={<StateChip kind="partial" detail={`${runtime.value.unavailable} unavailable attempts`} />}
      >
        <p className="text-3xs leading-snug text-text-muted">
          Attempt coverage is partial, so tasks without a returned attempt cannot be called
          unattributed. The region counts above are lower bounds.
        </p>
      </Panel>
    );
  }
  return (
    <Panel
      legend="Unattributed work"
      actions={
        <StateChip
          kind={reading.unattributed.length === 0 ? 'complete_zero_findings' : 'partial'}
          detail={`${reading.unattributed.length}`}
        />
      }
    >
      {reading.unattributed.length === 0 ? (
        <EmptyReading>
          {reading.taskCount === 0
            ? 'The snapshot returned no tasks, so there is no unattributed work to hold.'
            : 'Every task the snapshot returned has at least one attributed attempt, so the regions account for the whole page.'}
        </EmptyReading>
      ) : (
        <div
          className="flex min-w-0 flex-col gap-2"
          data-work-unattributed={reading.unattributed.length}
        >
          <p className="text-3xs leading-snug text-text-muted">
            The exact graph attributes no attempt to these tasks. They are held outside every region
            rather than assigned to one, and drawn hollow because the run that would name an
            executor for them does not exist in this read.
          </p>
          <ul className="flex min-w-0 flex-wrap gap-1.5">
            {reading.unattributed.map((task) => (
              <li key={task.taskId} className="min-w-0">
                <HollowMark
                  taskId={task.taskId}
                  title={task.title}
                  selected={selected === task.taskId}
                  onSelect={onSelect}
                />
              </li>
            ))}
          </ul>
        </div>
      )}
    </Panel>
  );
}

function HollowMark({
  taskId,
  title,
  selected,
  onSelect,
}: {
  taskId: string;
  title: string;
  selected: boolean;
  onSelect: (taskId: string) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => onSelect(taskId)}
      aria-pressed={selected}
      // 44px explicitly rather than a spacing utility: this app's root font
      // size is 14px, so `min-h-11` computes to 38.5px and lands under the
      // target size the accessibility gate measures.
      //
      // No fill in either state. Hollow is the reading, so selection moves the
      // outline rather than filling the mark in.
      className={cn(
        'flex min-h-[44px] min-w-0 max-w-[16rem] flex-col justify-center gap-0.5 border border-dashed px-2 py-1 text-left',
        'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent',
        selected ? 'border-accent' : 'border-edge hover:border-edge-strong',
      )}
      data-work-task={taskId}
      data-work-hollow="true"
    >
      <span className="min-w-0 truncate text-2xs text-text-secondary">{title}</span>
      <span className="truncate font-mono text-3xs text-text-muted">{taskId}</span>
    </button>
  );
}
