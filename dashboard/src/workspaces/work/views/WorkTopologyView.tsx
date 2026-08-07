import type {
  WorkAttemptListCoverageV1,
  WorkAttemptListV1,
  WorkProjectionSnapshotV1,
} from '../../../contracts/index.ts';
import { StateChip } from '../../../ui/StateChip.tsx';
import { Meter, Panel } from '../../../ui/instrument.tsx';
import { cn } from '../../../ui/cn.ts';
import { kindColorVars } from '../../../viz/graph/kindColor.ts';
import type { WorkResult } from '../workApi.ts';
import type { WorkChannel } from '../workChannel.ts';
import {
  WORK_TOPOLOGY_DIMENSIONS,
  topologyDimensionLabel,
  workTopologyReading,
  type WorkTopologyLanding,
  type WorkTopologyReading,
  type WorkTopologyThread,
  type WorkTopologyWorktreeLane,
} from '../workTopologyModel.ts';
import { ChannelAbsence, EmptyReading, ViewCaption } from './WorkViewChannel.tsx';

/**
 * Execution topology — where the work actually ran.
 *
 * The lens over the canonical Work selection that Plan 11 adds with the Work
 * delivery. Its full contract decodes `ExecutionTopologyViewV1`; this build's
 * generated catalog does not carry that DTO, so the lens draws exactly what
 * the mounted attempt read proves — the executor weave and the worktree lanes
 * of the `execution_placement` dimension, pinned to the verified topology
 * generation the page was read under — and states the other three dimensions
 * as the typed schema absences they are. `workTopologyModel.ts` is the binding
 * point for the real view when it lands; nothing here will need to un-learn a
 * fabricated lane.
 *
 * The weave is 11c's loom row: executors as warp threads, hue hashed from the
 * stable executor identity (the same provider route wears the same hue on
 * every screen of the app), tasks as landings, a retry as a repeated crossing
 * of the same landing. Every mark is hollow because the record holds an end
 * and never a start; the absence is printed where the axis would have been.
 *
 * Selection is the canonical one: a landing names a task, clicking it selects
 * that task everywhere, and the lens never owns the selection.
 */

/** Which of the readings the lens is drawing. The middle case is a page that
 * answered and held nothing — a statement, not an empty field. */
type TopologyBodyState = 'unread' | 'empty_page' | 'placed';

function topologyBodyState(reading: WorkTopologyReading): TopologyBodyState {
  if (reading.attempts.state !== 'listed') return 'unread';
  return reading.threads.available ? 'placed' : 'empty_page';
}

function coverageSentence(coverage: WorkAttemptListCoverageV1): string {
  switch (coverage.coverage) {
    case 'complete':
      return `${coverage.returned} ${coverage.returned === 1 ? 'attempt' : 'attempts'} · complete`;
    case 'capped':
      return `${coverage.returned} of ${coverage.returned + coverage.remaining} attempts · capped, every count a floor`;
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}

export function WorkTopologyView({
  snapshot,
  attemptList,
  selected,
  onSelect,
}: {
  snapshot: WorkProjectionSnapshotV1;
  attemptList: WorkResult<WorkAttemptListV1> | undefined;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const reading = workTopologyReading(attemptList);
  const titles = new Map(snapshot.projections.map((p) => [p.task_id, p.title]));

  return (
    <div className="flex min-w-0 flex-col gap-3" data-work-view="topology">
      <Panel
        legend="Execution topology"
        actions={<TopologyBindingChip reading={reading} />}
        elevation="well"
      >
        <div className="flex min-w-0 flex-col gap-3">
          <TopologyCaption reading={reading} />

          <p className="text-3xs leading-snug text-text-muted">
            Warp threads are executors — provider routes, each wearing the hue its stable
            identity hashes to everywhere in this app. Landings are tasks; repeated ticks on
            one landing are repeated crossings, which is a retry as the weave draws one.
            Every mark is hollow: an attempt records the instant it finished and nothing
            records when it started, so no thread here has a width to fill.
          </p>

          {/* The absence sits where a time axis would have been drawn. */}
          <ChannelAbsence measure="wall-clock spans and durations" channel={reading.wallClock} />

          <TopologyBody reading={reading} titles={titles} selected={selected} onSelect={onSelect} />
        </div>
      </Panel>

      <WorktreeLanes reading={reading} titles={titles} selected={selected} onSelect={onSelect} />

      <DimensionLedger reading={reading} />
    </div>
  );
}

/** The identity this lens pins: the verified topology generation the attempt
 * page was read under. Rendered as the panel's state chip so the pin and the
 * read state are one mark — an unpinned lens is exactly an unread one. */
function TopologyBindingChip({ reading }: { reading: WorkTopologyReading }) {
  const binding = reading.binding;
  if (binding.available) {
    return (
      <span
        className="td-value text-3xs text-text-muted"
        data-work-topology-generation={binding.value.generation}
        data-cell="numeric"
      >
        generation {binding.value.generation} · {binding.value.task_count} tasks
      </span>
    );
  }
  return <StateChip kind={binding.state} detail="topology generation" />;
}

function TopologyCaption({ reading }: { reading: WorkTopologyReading }) {
  const threads = reading.threads.available ? reading.threads.value : [];
  const lanes = reading.worktreeLanes.available ? reading.worktreeLanes.value : [];
  const landings = threads.reduce((total, thread) => total + thread.landings.length, 0);
  const crossings = threads.reduce((total, thread) => total + thread.attempts, 0);
  return (
    <ViewCaption
      population={`${threads.length} executors · ${landings} landings · ${crossings} crossings · ${lanes.length} worktree lanes`}
      note={
        reading.coverage.available ? coverageSentence(reading.coverage.value) : undefined
      }
    />
  );
}

function TopologyBody({
  reading,
  titles,
  selected,
  onSelect,
}: {
  reading: WorkTopologyReading;
  titles: ReadonlyMap<string, string>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const state = topologyBodyState(reading);
  switch (state) {
    case 'unread':
      // The read's own reason, in the daemon's taxonomy: pending, refused, or
      // the typed absence. Never an empty weave.
      return (
        <ChannelAbsence measure="the executor placement weave" channel={asAbsent(reading.threads)} />
      );
    case 'empty_page':
      return (
        <EmptyReading>
          The attempt page was read under this topology generation and holds no attempts, so
          no executor has placed anything. This is an authorized empty execution record, not
          a lens that failed to draw.
        </EmptyReading>
      );
    case 'placed':
      return (
        <ExecutorWeave
          threads={reading.threads.available ? reading.threads.value : []}
          titles={titles}
          selected={selected}
          onSelect={onSelect}
        />
      );
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

/** Narrow a channel to its absent half for `ChannelAbsence`. The one caller
 * above only reaches this when the reading is not `listed`, where the channel
 * is provably absent; the fallback keeps the narrowing total anyway. */
function asAbsent(channel: WorkChannel<unknown>): WorkChannel<never> {
  if (!channel.available) return channel;
  return {
    available: false,
    state: 'unknown',
    detail: 'the channel answered while the read did not — an inconsistency worth reporting',
  };
}

function ExecutorWeave({
  threads,
  titles,
  selected,
  onSelect,
}: {
  threads: readonly WorkTopologyThread[];
  titles: ReadonlyMap<string, string>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  // Threads arrive sorted by attempt count, so the ceiling is the first row.
  const ceiling = Math.max(1, threads[0]?.attempts ?? 1);
  return (
    <ol
      className="flex min-w-0 flex-col gap-1.5"
      data-work-executors={threads.length}
      data-work-span="hollow"
    >
      {threads.map((thread) => (
        <li key={thread.executorKey} className="min-w-0">
          <ExecutorThread
            thread={thread}
            ceiling={ceiling}
            titles={titles}
            selected={selected}
            onSelect={onSelect}
          />
        </li>
      ))}
    </ol>
  );
}

/**
 * One warp thread: an executor, its admission evidence, and its landings.
 *
 * The hue mark is hashed from the executor identity — provider and route —
 * which IS a stable app-wide identity, unlike the run-keyed thread of the
 * timeline weave. It is the only solid ink on the row: identity, not a span.
 */
function ExecutorThread({
  thread,
  ceiling,
  titles,
  selected,
  onSelect,
}: {
  thread: WorkTopologyThread;
  ceiling: number;
  titles: ReadonlyMap<string, string>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const notes: string[] = [];
  if (thread.diverted > 0) notes.push(`${thread.diverted} diverted here by fallback`);
  if (thread.unobserved > 0) notes.push(`${thread.unobserved} not yet observed to run here`);
  return (
    <div
      className="flex min-w-0 flex-col gap-1.5 border border-edge-subtle bg-surface-1 p-2"
      data-work-executor={thread.executorKey}
      data-work-attempts={thread.attempts}
    >
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        <span
          aria-hidden
          style={kindColorVars(thread.executorKey)}
          className="h-3 w-1 shrink-0 bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
        />
        <span className="min-w-0 flex-1 truncate font-mono text-2xs text-text-secondary">
          {thread.executorKey}
        </span>
        <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
          {thread.attempts} {thread.attempts === 1 ? 'attempt' : 'attempts'} ·{' '}
          {thread.backends.join(', ')} · {thread.models.join(', ')}
        </span>
      </div>

      {notes.length > 0 ? (
        <p className="text-3xs leading-snug text-text-muted">{notes.join(' · ')}</p>
      ) : null}

      {/* The attempt count a second time as a length, so a stack of threads
        * ranks without reading digits. Hidden: the figure is printed above. */}
      <Meter fraction={thread.attempts / ceiling} height="row" />

      <ul className="flex min-w-0 flex-wrap items-stretch gap-1">
        {thread.landings.map((landing) => (
          <li key={landing.taskId} className="min-w-0">
            <TopologyLandingMark
              landing={landing}
              executorKey={thread.executorKey}
              title={titles.get(landing.taskId) ?? landing.taskId}
              selected={selected === landing.taskId}
              onSelect={onSelect}
            />
          </li>
        ))}
      </ul>
    </div>
  );
}

/** Ticks stop repeating past this; the printed count carries the rest. */
const TALLY_CAP = 6;

function TopologyLandingMark({
  landing,
  executorKey,
  title,
  selected,
  onSelect,
}: {
  landing: WorkTopologyLanding;
  executorKey: string;
  title: string;
  selected: boolean;
  onSelect: (taskId: string) => void;
}) {
  const ticks = Math.min(landing.crossings, TALLY_CAP);
  return (
    <button
      type="button"
      onClick={() => onSelect(landing.taskId)}
      aria-pressed={selected}
      aria-label={`${title} · ${landing.crossings} ${
        landing.crossings === 1 ? 'crossing' : 'crossings'
      } by ${executorKey}${landing.open ? ' · open' : ''}`}
      data-work-task={landing.taskId}
      data-work-crossings={landing.crossings}
      className={cn(
        // Hollow on purpose: a landing is an incidence, not a span.
        'flex min-h-[44px] min-w-0 flex-col justify-center gap-1 border bg-transparent px-2 py-1 text-left',
        'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
        selected
          ? 'border-edge-strong text-text-primary'
          : 'border-edge-subtle text-text-secondary hover:bg-surface-2',
      )}
    >
      <span className="max-w-44 truncate text-2xs">{title}</span>
      <span className="flex items-center gap-1" aria-hidden>
        {Array.from({ length: ticks }, (_, index) => (
          <span
            key={index}
            className={cn('h-1.5 w-1.5 border', landing.terminal ? 'border-edge-strong' : 'border-edge-subtle')}
          />
        ))}
        {landing.crossings > TALLY_CAP ? (
          <span className="text-3xs text-text-muted">×{landing.crossings}</span>
        ) : null}
        {landing.open ? <span className="text-3xs text-text-muted">open</span> : null}
      </span>
    </button>
  );
}

/**
 * The worktree lanes: every attempt pinned to the exact repository, worktree,
 * ref, and commit its execution envelope was admitted against. These are the
 * placement identities Plan 11 says the lens pins — read, not inferred, and a
 * `null` ref is printed as the recorded absence it is.
 */
function WorktreeLanes({
  reading,
  titles,
  selected,
  onSelect,
}: {
  reading: WorkTopologyReading;
  titles: ReadonlyMap<string, string>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const lanes = reading.worktreeLanes;
  return (
    <Panel legend="Worktree lanes">
      {lanes.available ? (
        <ol className="flex min-w-0 flex-col gap-2" data-work-worktree-lanes={lanes.value.length}>
          {lanes.value.map((lane) => (
            <li key={lane.worktreeId} className="min-w-0">
              <WorktreeLaneRow lane={lane} titles={titles} selected={selected} onSelect={onSelect} />
            </li>
          ))}
        </ol>
      ) : (
        <ChannelAbsence measure="worktree placement" channel={lanes} />
      )}
    </Panel>
  );
}

function WorktreeLaneRow({
  lane,
  titles,
  selected,
  onSelect,
}: {
  lane: WorkTopologyWorktreeLane;
  titles: ReadonlyMap<string, string>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  return (
    <div
      className="flex min-w-0 flex-col gap-1.5 border border-edge-subtle bg-surface-1 p-2"
      data-work-worktree={lane.worktreeId}
    >
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        <span className="min-w-0 truncate font-mono text-2xs text-text-secondary">
          {lane.worktreeId}
        </span>
        <span className="min-w-0 flex-1 truncate text-3xs text-text-muted">{lane.worktreeRoot}</span>
        <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
          {lane.attempts} {lane.attempts === 1 ? 'attempt' : 'attempts'} · repository{' '}
          {lane.repositoryIds.join(', ')}
        </span>
      </div>

      <ul className="flex min-w-0 flex-wrap gap-x-3 gap-y-1 text-3xs text-text-muted">
        {lane.refs.map((pin) => (
          <li
            key={`${pin.reference ?? 'none'}-${pin.commit}`}
            className="min-w-0 truncate font-mono"
            data-work-ref={pin.reference ?? 'none'}
            data-work-commit={pin.commit}
          >
            {pin.reference ?? 'no ref recorded'} @ {pin.commit}
          </li>
        ))}
      </ul>

      <ul className="flex min-w-0 flex-wrap items-center gap-1">
        {lane.taskIds.map((taskId) => (
          <li key={taskId} className="min-w-0">
            <button
              type="button"
              onClick={() => onSelect(taskId)}
              aria-pressed={selected === taskId}
              data-work-task={taskId}
              className={cn(
                'min-h-[44px] max-w-44 truncate border px-2 text-left text-2xs',
                'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
                selected === taskId
                  ? 'border-edge-strong text-text-primary'
                  : 'border-edge-subtle text-text-secondary hover:bg-surface-2',
              )}
            >
              {titles.get(taskId) ?? taskId}
            </button>
          </li>
        ))}
        {lane.executorKeys.map((executorKey) => (
          <li key={executorKey} className="flex min-w-0 items-center gap-1" aria-hidden>
            <span
              style={kindColorVars(executorKey)}
              className="h-3 w-1 shrink-0 bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
            />
            <span className="truncate font-mono text-3xs text-text-muted">{executorKey}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * The four dimensions, every one on screen in every state. Plan 11:
 * unsupported, unavailable, denied, partial, stale, or omitted lane families
 * remain explicit and never disappear into the base board. The readable one
 * states what it is read from; the three the catalog cannot answer wear their
 * `unsupported_schema` chips here, beside it, rather than in a footnote.
 */
function DimensionLedger({ reading }: { reading: WorkTopologyReading }) {
  return (
    <section
      aria-label="Topology dimensions"
      className="flex min-w-0 flex-col gap-2 border border-edge-subtle bg-surface-2 p-2.5"
      data-work-topology-dimensions={WORK_TOPOLOGY_DIMENSIONS.length}
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate text-text-secondary">Topology dimensions</h3>
        <span aria-hidden className="td-rule" />
      </div>
      <ul className="flex min-w-0 flex-col gap-2">
        {WORK_TOPOLOGY_DIMENSIONS.map((dimension) => (
          <li key={dimension} className="min-w-0" data-work-dimension={dimension}>
            <DimensionRow reading={reading} dimension={dimension} />
          </li>
        ))}
      </ul>
    </section>
  );
}

function DimensionRow({
  reading,
  dimension,
}: {
  reading: WorkTopologyReading;
  dimension: (typeof WORK_TOPOLOGY_DIMENSIONS)[number];
}) {
  switch (dimension) {
    case 'execution_placement':
      return (
        <div className="flex min-w-0 flex-col gap-1">
          {reading.threads.available ? (
            <>
              <StateChip kind="ready" detail={topologyDimensionLabel(dimension)} />
              <p className="text-3xs leading-snug text-text-muted">
                read from the mounted attempt list: each attempt&apos;s execution envelope
                names its executor route and the exact repository, worktree, ref, and commit
                it was admitted against
              </p>
            </>
          ) : (
            <ChannelAbsence
              measure={topologyDimensionLabel(dimension)}
              channel={asAbsent(reading.threads)}
            />
          )}
        </div>
      );
    case 'branch_topology':
      return (
        <ChannelAbsence
          measure={topologyDimensionLabel(dimension)}
          channel={reading.branchTopology}
        />
      );
    case 'review_topology':
      return (
        <ChannelAbsence
          measure={topologyDimensionLabel(dimension)}
          channel={reading.reviewTopology}
        />
      );
    case 'integration_strategy':
      return (
        <ChannelAbsence
          measure={topologyDimensionLabel(dimension)}
          channel={reading.integrationStrategy}
        />
      );
    default: {
      const unhandled: never = dimension;
      return unhandled;
    }
  }
}
