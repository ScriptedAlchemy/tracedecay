import type { WorkProjectionSnapshotV1 } from '../../../contracts/index.ts';
import { StateChip } from '../../../ui/StateChip.tsx';
import { Meter, Panel } from '../../../ui/instrument.tsx';
import { cn } from '../../../ui/cn.ts';
import { formatMicrosUtc } from '../../../ui/format.ts';
import { kindColorVars } from '../../../viz/graph/kindColor.ts';
import { coverageReading } from '../workModel.ts';
import type { WorkAttemptReading } from '../workAttemptModel.ts';
import type { WorkGraphReading } from '../workGraphModel.ts';
import {
  type WorkWeaveLanding,
  type WorkWeaveReading,
  type WorkWeaveThread,
  workWeaveReading,
} from '../workViewsModel.ts';
import { WorkExecutionRecord } from './WorkExecutionRecord.tsx';
import { ChannelAbsence, ChannelLedger, EmptyReading, ViewCaption } from './WorkViewChannel.tsx';

/**
 * Timeline / attempts — the loom weave over the run/task incidence.
 *
 * Warp threads are runs and landings are tasks, so a retry is exactly what it
 * is on a loom: the same thread crossing the same landing again. A thread wears
 * a stable hue so neighbours stay tellable apart and carries no label but its
 * run id, because `run_id` is the only executor-shaped identity this read
 * holds and it is not an executor. `executorIdentity` is the absence beside it.
 *
 * Every mark is hollow, and that is the reading rather than a style. An
 * evidence reference is a point with no start, `WorkProjection` carries no
 * timestamp anywhere, and the Loom weave already draws a zero-extent reading as
 * outline only. Here the zero extent is not one thread's gap but the whole
 * field: no axis, no span, no duration, and position along a thread is task-id
 * collation order rather than an order anything ran in. A time axis over this
 * data would be the one mark on the page nobody could check.
 *
 * Tasks no run has landed on hold their own band. Dropping them would make the
 * weave look complete, and placing them at ordinal zero would put them on a
 * thread that never crossed them.
 *
 * Accessibility. The weave is an ordered list of threads each holding a list of
 * buttons, so the visualization IS the accessible structure: it takes Tab in
 * reading order, names every landing with its task and its run, and needs no
 * parallel text twin. Tally ticks and crossing rails repeat a figure printed
 * beside them and stay out of the accessibility tree.
 */

/** Which of the three readings the weave panel is drawing. The degenerate
 * middle case is a statement, not an empty field. */
type WeaveState = 'empty_board' | 'unwoven_board' | 'woven';

function weaveState(taskCount: number, threadCount: number): WeaveState {
  if (taskCount === 0) return 'empty_board';
  if (threadCount === 0) return 'unwoven_board';
  return 'woven';
}

/** Ticks stop repeating past this and the printed count carries the rest. The
 * tally is decoration of a figure set beside it, so a capped row costs nothing;
 * a row that widened without bound would cost the thread its shape. */
const TALLY_CAP = 6;

export function WorkTimelineView({
  snapshot,
  attempts,
  graph,
  selected,
  onSelect,
}: {
  snapshot: WorkProjectionSnapshotV1;
  attempts: WorkAttemptReading;
  graph: WorkGraphReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const reading = workWeaveReading(snapshot.projections, attempts, graph);
  const coverage = coverageReading(snapshot.coverage);

  const landings = reading.threads.reduce((total, thread) => total + thread.landings.length, 0);
  const retried = reading.threads.reduce(
    (total, thread) => total + thread.landings.filter((landing) => landing.crossings > 1).length,
    0,
  );

  const notes: string[] = [];
  if (retried > 0) {
    notes.push(`${retried} ${retried === 1 ? 'landing' : 'landings'} crossed more than once`);
  }
  if (reading.threads.length === 1) notes.push('one thread carries every crossing');

  return (
    <div className="flex min-w-0 flex-col gap-3" data-work-view="timeline">
      <Panel
        legend="Attempt weave"
        actions={<StateChip kind={coverage.state} detail={coverage.detail} />}
        elevation="well"
      >
        <div className="flex min-w-0 flex-col gap-3">
          <ViewCaption
            population={`${reading.threads.length} threads · ${landings} landings · ${reading.crossings} crossings`}
            note={notes.length > 0 ? notes.join(' · ') : undefined}
          />

          <p className="text-3xs leading-snug text-text-muted">
            Position along a thread is task-id order and not an order of execution. Every
            mark is drawn hollow because an evidence reference is a point: no landing here
            has a start, an end, or a width. Repeated ticks on one mark are repeated
            crossings of the same landing, which looks like a retry and is not measured as
            one — the measured retry chains are in the execution record below, read from the
            attempt list rather than counted off this incidence.
          </p>

          {/* The absence sits where an axis would have been drawn, not in a
            * footnote, so a field with no time in it cannot be read as a field
            * whose time is merely off-screen. */}
          <ChannelAbsence measure="wall-clock spans and durations" channel={reading.wallClock} />

          <WeaveBody
            reading={reading}
            taskCount={snapshot.projections.length}
            selected={selected}
            onSelect={onSelect}
          />
        </div>
      </Panel>

      <WorkExecutionRecord reading={reading} selected={selected} onSelect={onSelect} />

      <RecordedInstants reading={reading} />

      <div className="grid min-w-0 gap-3 lg:grid-cols-2">
        <UnwovenBand reading={reading} selected={selected} onSelect={onSelect} />
        {/* Wall clock alone. The attempt-derived channels are drawn in the
          * execution record, each where its measurement would have been: a
          * refused attempt read makes all four absent for one reason, and
          * repeating that one sentence in a ledger as well would print it four
          * times on a page that has one thing to say. The recorded instants get
          * the same treatment, in their own panel above. */}
        <ChannelLedger
          legend="Measurements this projection could not take"
          channels={[
            { measure: 'wall-clock spans and durations', channel: reading.wallClock },
          ]}
        />
      </div>
    </div>
  );
}

function WeaveBody({
  reading,
  taskCount,
  selected,
  onSelect,
}: {
  reading: WorkWeaveReading;
  taskCount: number;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const state = weaveState(taskCount, reading.threads.length);
  switch (state) {
    case 'empty_board':
      return (
        <EmptyReading>
          The snapshot returned no tasks, so there is nothing to weave. This is the daemon
          reporting an empty board, not a projection that failed to draw.
        </EmptyReading>
      );
    case 'unwoven_board':
      return (
        <EmptyReading>
          The snapshot returned {taskCount} {taskCount === 1 ? 'task' : 'tasks'} and no run
          has attached evidence to any of them, so the weave has no threads at all. Every
          returned task sits in the unwoven band below. A distribution this degenerate is
          said rather than drawn as a field that looks like a failed render.
        </EmptyReading>
      );
    case 'woven':
      return <Weave reading={reading} selected={selected} onSelect={onSelect} />;
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

function Weave({
  reading,
  selected,
  onSelect,
}: {
  reading: WorkWeaveReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  // Threads arrive sorted by crossing count, so the ceiling is the first row.
  const ceiling = Math.max(1, reading.threads[0]?.crossings ?? 1);
  return (
    <ol
      className="flex min-w-0 flex-col gap-1.5"
      data-work-threads={reading.threads.length}
      data-work-span="hollow"
    >
      {reading.threads.map((thread) => (
        <li key={thread.runId} className="min-w-0">
          <Thread
            thread={thread}
            ceiling={ceiling}
            selected={selected}
            onSelect={onSelect}
          />
        </li>
      ))}
    </ol>
  );
}

/**
 * One warp thread: a run, its two counts, and the landings it crossed.
 *
 * The hue mark is a stable hash of the run id and claims nothing further. It
 * separates this thread from the one under it; it does not name a person, an
 * agent, a model, or a provider, and no reading here could supply one. That
 * mark is the only solid ink on the row, because it is an identity rather than
 * a span.
 */
function Thread({
  thread,
  ceiling,
  selected,
  onSelect,
}: {
  thread: WorkWeaveThread;
  ceiling: number;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  return (
    <div
      className="flex min-w-0 flex-col gap-1.5 border border-edge-subtle bg-surface-1 p-2"
      data-work-thread={thread.runId}
      data-work-crossings={thread.crossings}
      data-work-terminal-landings={thread.terminalLandings}
    >
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        <span
          aria-hidden
          style={kindColorVars(thread.runId)}
          className="h-3 w-1 shrink-0 bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
        />
        <span className="min-w-0 flex-1 truncate font-mono text-2xs text-text-secondary">
          {thread.runId}
        </span>
        <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
          {thread.crossings} {thread.crossings === 1 ? 'crossing' : 'crossings'} ·{' '}
          {thread.terminalLandings} of {thread.landings.length} terminal
        </span>
      </div>

      {/* The crossing count a second time as a length, so a stack of threads
        * ranks without reading digits. Hidden: the figure is printed above. */}
      <Meter fraction={thread.crossings / ceiling} height="row" />

      <ul className="flex min-w-0 flex-wrap items-stretch gap-1">
        {thread.landings.map((landing, index) => (
          <li key={landing.taskId} className="min-w-0">
            <Landing
              landing={landing}
              runId={thread.runId}
              ordinal={index + 1}
              total={thread.landings.length}
              selected={selected === landing.taskId}
              onSelect={onSelect}
            />
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * One landing, drawn hollow.
 *
 * Terminal is carried by a printed word as well as by the border hue, so the
 * distinction survives a monochrome rendering. Selection is a lamp down the
 * leading edge rather than a fill, because filling the mark is the one thing
 * this view is not allowed to do — a solid mark would claim a measured extent.
 */
function Landing({
  landing,
  runId,
  ordinal,
  total,
  selected,
  onSelect,
}: {
  landing: WorkWeaveLanding;
  runId: string;
  ordinal: number;
  total: number;
  selected: boolean;
  onSelect: (taskId: string) => void;
}) {
  const word = landing.terminal ? 'terminal' : 'open';
  const crossings = `${landing.crossings} ${landing.crossings === 1 ? 'crossing' : 'crossings'}`;
  return (
    <button
      type="button"
      onClick={() => onSelect(landing.taskId)}
      aria-pressed={selected}
      // The visible face of a landing is a tally and two words, which names no
      // task on its own, so the button carries the whole reading as its name.
      aria-label={`${landing.title} — task ${landing.taskId}, position ${ordinal} of ${total} in task-id order on run ${runId}, ${crossings}, ${landing.terminal ? 'terminal evidence' : 'no terminal evidence'}`}
      // 44px explicitly rather than a spacing utility: this app's root font
      // size is 14px, so `min-h-11` computes to 38.5px and lands under the
      // target size the accessibility gate measures.
      className={cn(
        'relative flex min-h-[44px] min-w-0 max-w-[14rem] flex-col justify-center gap-0.5',
        'border bg-transparent px-2 py-1 text-left',
        'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent',
        selected
          ? 'border-accent'
          : landing.terminal
            ? 'border-state-ready'
            : 'border-edge-subtle',
        'hover:border-edge-strong',
      )}
      // `data-work-task` is the hook every projection puts on a control that
      // moves the canonical selection, so selection synchronization is
      // assertable across all of them by one query.
      data-work-task={landing.taskId}
      data-work-landing={landing.taskId}
      data-work-crossings={landing.crossings}
      data-work-terminal={landing.terminal ? 'true' : undefined}
    >
      {selected ? (
        <span aria-hidden className="absolute inset-y-0 left-0 w-[2px] bg-accent" />
      ) : null}
      <span className="flex min-w-0 items-center gap-1.5">
        <Tally count={landing.crossings} runId={runId} />
        <span className="min-w-0 truncate text-2xs text-text-primary">{landing.title}</span>
      </span>
      <span className="truncate text-3xs text-text-muted">
        {word} · {crossings}
      </span>
    </button>
  );
}

/** One hollow tick per crossing, in the thread's hue. Repeated rather than
 * scaled, because a retry is a second crossing and not a bigger one. */
function Tally({ count, runId }: { count: number; runId: string }) {
  const drawn = Math.min(count, TALLY_CAP);
  return (
    <span
      aria-hidden
      style={kindColorVars(runId)}
      className="flex shrink-0 items-center gap-px"
    >
      {Array.from({ length: drawn }, (_, index) => (
        <span
          key={index}
          className="h-3 w-1 border border-[var(--kind-dark)] [[data-theme=light]_&]:border-[var(--kind-light)]"
        />
      ))}
    </span>
  );
}

/**
 * The four instants the work-product graph records per task.
 *
 * A calendar, and deliberately not an axis. Created, last changed, scheduled
 * for, due by — four points, none of which is the start of an attempt, so
 * nothing here closes the wall-clock absence stated at the top of the weave.
 * They are drawn as a table of instants rather than laid on a time line for
 * exactly that reason: a line would invite reading the distance between two
 * marks as a duration something took.
 *
 * `scheduled_at` and `deadline` are nullable in the contract and their absence
 * is printed as an absence rather than as an epoch: a task with no deadline is
 * not a task due at the beginning of time.
 */
function RecordedInstants({ reading }: { reading: WorkWeaveReading }) {
  const instants = reading.instants;
  return (
    <Panel
      legend="Instants the graph records"
      actions={
        instants.available ? (
          <StateChip
            kind={instants.value.length === 0 ? 'complete_zero_findings' : 'ready'}
            detail={`${instants.value.length}`}
          />
        ) : (
          <StateChip kind={instants.state} detail="not read" />
        )
      }
      bodyClassName={instants.available && instants.value.length > 0 ? 'p-0' : undefined}
    >
      {!instants.available ? (
        <p className="text-3xs leading-snug text-text-muted">{instants.detail}</p>
      ) : instants.value.length === 0 ? (
        <EmptyReading>
          The work-product graph version this read returned holds no task at all, so there is no
          instant to record. This is the authority reporting an empty graph, not a read that
          failed.
        </EmptyReading>
      ) : (
        <div
          role="region"
          aria-label="Recorded task instants"
          tabIndex={0}
          className="min-w-0 overflow-x-auto"
          data-work-instants={instants.value.length}
        >
          <table className="w-full min-w-0 border-collapse text-2xs">
            <caption className="sr-only">
              Every task the work-product graph returned, with the instant it was created, the
              instant it last changed, the instant it is scheduled for and the instant it is due
              by. None of these is the start of an attempt, so no duration is derivable from
              them.
            </caption>
            <thead>
              <tr className="border-b border-edge text-text-muted">
                <th scope="col" className="px-2 py-1 text-left font-medium">
                  Task
                </th>
                <th scope="col" className="px-2 py-1 text-left font-medium">
                  Created
                </th>
                <th scope="col" className="px-2 py-1 text-left font-medium">
                  Last changed
                </th>
                <th scope="col" className="px-2 py-1 text-left font-medium">
                  Scheduled
                </th>
                <th scope="col" className="px-2 py-1 text-left font-medium">
                  Due
                </th>
              </tr>
            </thead>
            <tbody>
              {instants.value.map((row) => (
                <tr key={row.taskId} data-work-instant={row.taskId}>
                  <th
                    scope="row"
                    className="min-w-0 px-2 py-1 text-left align-top font-normal font-mono text-3xs text-text-secondary"
                  >
                    {row.taskId}
                  </th>
                  <td className="px-2 py-1 align-top font-mono text-3xs text-text-muted">
                    {formatMicrosUtc(row.createdAt)}
                  </td>
                  <td className="px-2 py-1 align-top font-mono text-3xs text-text-secondary">
                    {formatMicrosUtc(row.updatedAt)}
                  </td>
                  <td className="px-2 py-1 align-top font-mono text-3xs text-text-muted">
                    {row.scheduledAt === null ? 'not scheduled' : formatMicrosUtc(row.scheduledAt)}
                  </td>
                  <td className="px-2 py-1 align-top font-mono text-3xs text-text-muted">
                    {row.deadline === null ? 'no deadline' : formatMicrosUtc(row.deadline)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Panel>
  );
}

/**
 * The unscheduled band.
 *
 * A task no run has landed on is a reading about the board, so it gets a band
 * of its own and is never quietly left out of the weave. It is drawn hollow
 * like every other mark here, and for the same reason.
 */
function UnwovenBand({
  reading,
  selected,
  onSelect,
}: {
  reading: WorkWeaveReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  return (
    <Panel
      legend="Unwoven band"
      actions={
        <StateChip
          kind={reading.unwoven.length === 0 ? 'complete_zero_findings' : 'partial'}
          detail={`${reading.unwoven.length}`}
        />
      }
    >
      {reading.unwoven.length === 0 ? (
        <EmptyReading>
          Every task the snapshot returned carries evidence from at least one run, so no
          task stands outside the weave.
        </EmptyReading>
      ) : (
        <div className="flex min-w-0 flex-col gap-2">
          <p className="text-3xs leading-snug text-text-muted">
            No run has attached evidence to these tasks. They occupy this band rather than
            being omitted from the weave: a task nothing has crossed is a reading, and
            leaving it out would make the threads above look like the whole board.
          </p>
          <ul
            className="flex min-w-0 flex-wrap gap-1"
            data-work-unwoven={reading.unwoven.length}
          >
            {reading.unwoven.map((task) => (
              <li key={task.taskId} className="min-w-0">
                <button
                  type="button"
                  onClick={() => onSelect(task.taskId)}
                  aria-pressed={selected === task.taskId}
                  aria-label={`${task.title} — task ${task.taskId}, unwoven: no run has landed on it`}
                  className={cn(
                    'relative flex min-h-[44px] min-w-0 max-w-[14rem] flex-col justify-center gap-0.5',
                    'border border-dashed bg-transparent px-2 py-1 text-left',
                    'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent',
                    selected === task.taskId ? 'border-accent' : 'border-edge-subtle',
                    'hover:border-edge-strong',
                  )}
                  data-work-task={task.taskId}
                  data-work-unwoven-task={task.taskId}
                >
                  {selected === task.taskId ? (
                    <span aria-hidden className="absolute inset-y-0 left-0 w-[2px] bg-accent" />
                  ) : null}
                  <span className="min-w-0 truncate text-2xs text-text-primary">
                    {task.title}
                  </span>
                  <span className="truncate text-3xs text-text-muted">no crossings</span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Panel>
  );
}
