import type { WorkProjection, WorkProjectionSnapshotV1 } from '../../../contracts/index.ts';
import { StateChip } from '../../../ui/StateChip.tsx';
import { MeterRow, Panel } from '../../../ui/instrument.tsx';
import { cn } from '../../../ui/cn.ts';
import { kindColorVars } from '../../../viz/graph/kindColor.ts';
import { coverageReading } from '../workModel.ts';
import {
  type WorkloadReading,
  type WorkloadRegion,
  workloadReading,
} from '../workViewsModel.ts';
import { ChannelLedger, EmptyReading, ViewCaption } from './WorkViewChannel.tsx';

/**
 * Workload / executor / model — the cortex aggregation over runs.
 *
 * A cortex draws regions whose area is mass, whose contours are concurrency
 * and whose heat is recent churn. This build has none of those three. What
 * `WorkProjection` carries is the run/task incidence, so a region is a run and
 * its length is TASK COUNT — captioned as that, printed as that beside every
 * bar, and never called mass, cost or load. The three measurements plan 11c
 * asks the cortex to encode are drawn as the absences they are.
 *
 * No contour is drawn and no heat ramp appears anywhere on this page. The hue
 * that separates one region from its neighbour is the console's categorical
 * kind arc; a warm-to-cool ramp standing in for something that is not churn
 * would be read as churn by anyone who knows the grammar.
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

/** One task inside one region, with the evidence that run attached to it. */
interface RegionMember {
  readonly taskId: string;
  readonly title: string;
  readonly evidenceCount: number;
  readonly terminal: boolean;
}

/**
 * Which tasks each region holds.
 *
 * `WorkloadRegion` carries counts rather than members, and membership is
 * exactly the incidence those counts were taken over: a task falls in a run's
 * region when that run attached any evidence to it. One task can fall in
 * several regions, so the region counts are a reading per run and not a
 * partition of the board — which is why the bars are ranked against the
 * largest region rather than drawn as shares of a whole.
 */
function regionMembers(
  projections: readonly WorkProjection[],
): ReadonlyMap<string, readonly RegionMember[]> {
  const members = new Map<string, RegionMember[]>();
  for (const projection of projections) {
    const perRun = new Map<string, { evidenceCount: number; terminal: boolean }>();
    for (const evidence of projection.runtime_evidence) {
      const tally = perRun.get(evidence.run_id);
      perRun.set(evidence.run_id, {
        evidenceCount: (tally?.evidenceCount ?? 0) + 1,
        terminal: (tally?.terminal ?? false) || evidence.terminal,
      });
    }
    for (const [runId, tally] of perRun) {
      const member: RegionMember = {
        taskId: projection.task_id,
        title: projection.title,
        evidenceCount: tally.evidenceCount,
        terminal: tally.terminal,
      };
      const bucket = members.get(runId);
      if (bucket === undefined) members.set(runId, [member]);
      else bucket.push(member);
    }
  }
  for (const bucket of members.values()) {
    bucket.sort((a, b) => b.evidenceCount - a.evidenceCount || a.taskId.localeCompare(b.taskId));
  }
  return members;
}

export function WorkWorkloadView({
  snapshot,
  selected,
  onSelect,
}: {
  snapshot: WorkProjectionSnapshotV1;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const reading = workloadReading(snapshot.projections);
  const coverage = coverageReading(snapshot.coverage);
  const members = regionMembers(snapshot.projections);
  const attributed = reading.taskCount - reading.unattributed.length;

  return (
    <div className="flex min-w-0 flex-col gap-3" data-work-view="workload">
      <Panel
        legend="Runs as regions"
        actions={<StateChip kind={coverage.state} detail={coverage.detail} />}
        elevation="well"
      >
        <div className="flex min-w-0 flex-col gap-3">
          {/* The aggregation ratio leads the panel, because a cortex that does
            * not print how many things it folded into how few is a picture of
            * a number nobody can recover. */}
          <ViewCaption
            population={`${reading.taskCount} tasks ⟵ ${reading.regions.length} regions`}
            note={`${attributed} of ${reading.taskCount} in a region · ${reading.evidenceCount} evidence records`}
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
      </Panel>

      <div className="grid min-w-0 gap-3 lg:grid-cols-2">
        <Unattributed reading={reading} selected={selected} onSelect={onSelect} />
        <ChannelLedger
          legend="Measurements this projection could not take"
          channels={[
            { measure: 'task mass by effort', channel: reading.effortMass },
            { measure: 'concurrency contours', channel: reading.concurrency },
            { measure: 'recent churn', channel: reading.churn },
          ]}
        />
      </div>
    </div>
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
        No run has attached evidence to any task the snapshot returned, so no region exists
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
        {region.taskCount} {region.taskCount === 1 ? 'task' : 'tasks'} · {region.evidenceCount}{' '}
        evidence · {region.terminalCount} terminal · executor unnamed
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
      data-work-evidence={member.evidenceCount}
    >
      <span className="min-w-0 truncate text-2xs text-text-primary">{member.title}</span>
      <span className="truncate text-3xs text-text-muted">
        {member.evidenceCount} evidence in this region
        {member.terminal ? ' · terminal' : ''}
      </span>
    </button>
  );
}

/**
 * Work no run claims.
 *
 * These tasks carry no runtime evidence at all, so no region can hold them
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
            : 'Every task the snapshot returned carries evidence from at least one run, so the regions account for the whole page.'}
        </EmptyReading>
      ) : (
        <div
          className="flex min-w-0 flex-col gap-2"
          data-work-unattributed={reading.unattributed.length}
        >
          <p className="text-3xs leading-snug text-text-muted">
            No run has attached evidence to these tasks. They are held outside every region
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
