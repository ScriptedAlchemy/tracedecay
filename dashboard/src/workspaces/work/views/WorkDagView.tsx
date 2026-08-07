import type { WorkProjectionSnapshotV1 } from '../../../contracts/index.ts';
import { StateChip } from '../../../ui/StateChip.tsx';
import { Meter, Panel } from '../../../ui/instrument.tsx';
import { cn } from '../../../ui/cn.ts';
import { coverageReading } from '../workModel.ts';
import {
  type WorkDagComponent,
  type WorkDagReading,
  workDagReading,
} from '../workViewsModel.ts';
import { ChannelLedger, EmptyReading, ViewCaption } from './WorkViewChannel.tsx';

/**
 * DAG / critical path — the transit-map strata over the declared task graph.
 *
 * Strata are the longest path over the Tarjan condensation, the same discipline
 * the Code workspace layers imports with: a task sits one stratum below the
 * deepest thing it declares a dependency on, and a dependency cycle is
 * condensed into one mark rather than broken. Plan 11c calls a backward jump
 * the climb hue and requires the caption to state it is an observation; a
 * declared cycle is a real reading of the plan, not a rendering fault.
 *
 * The widest channel is the deepest chain of components. It is UNWEIGHTED —
 * 11c's critical path is weighted by effort, `WorkProjection` has no effort
 * field, and a chain weighted by a number this build invented would be the one
 * thing on the page nobody could check. The absence is drawn instead.
 *
 * Accessibility. The strata are an ordered list of ordered lists of buttons,
 * so the visualization IS the accessible structure: it takes Tab in reading
 * order, announces each task's depth and cycle membership, and needs no
 * parallel text twin. The channel rails beside each stratum are decoration of
 * a number printed next to them and stay out of the accessibility tree.
 */

export function WorkDagView({
  snapshot,
  selected,
  onSelect,
}: {
  snapshot: WorkProjectionSnapshotV1;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const reading = workDagReading(snapshot.projections);
  const coverage = coverageReading(snapshot.coverage);
  const onLongestChain = new Set(reading.longestChain.map((component) => component.index));

  return (
    <div className="flex min-w-0 flex-col gap-3" data-work-view="dag">
      <Panel
        legend="Declared dependency strata"
        actions={<StateChip kind={coverage.state} detail={coverage.detail} />}
        elevation="well"
      >
        <div className="flex min-w-0 flex-col gap-3">
          <ViewCaption
            population={`${snapshot.projections.length} tasks · ${reading.strata.length} strata · ${reading.edges.length} declared edges`}
            note={
              reading.longestChain.length > 0
                ? `deepest chain ${reading.longestChain.length} deep, unweighted`
                : undefined
            }
          />

          {snapshot.projections.length === 0 ? (
            <EmptyReading>
              The snapshot returned no tasks, so there is no graph to layer. This is the
              daemon reporting an empty board, not a projection that failed to draw.
            </EmptyReading>
          ) : (
            <Strata
              reading={reading}
              onLongestChain={onLongestChain}
              selected={selected}
              onSelect={onSelect}
            />
          )}
        </div>
      </Panel>

      <div className="grid min-w-0 gap-3 lg:grid-cols-2">
        <ClimbAndCycles reading={reading} onSelect={onSelect} />
        <div className="flex min-w-0 flex-col gap-3">
          <UnresolvedEdges reading={reading} />
          <ChannelLedger
            legend="Measurements this projection could not take"
            channels={[
              { measure: 'effort-weighted critical path', channel: reading.effort },
            ]}
          />
        </div>
      </div>
    </div>
  );
}

function Strata({
  reading,
  onLongestChain,
  selected,
  onSelect,
}: {
  reading: WorkDagReading;
  onLongestChain: ReadonlySet<number>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const widest = Math.max(1, reading.widestStratum);
  return (
    <ol className="flex min-w-0 flex-col gap-1.5" data-work-strata={reading.strata.length}>
      {reading.strata.map((stratum) => (
        <li
          key={stratum.depth}
          className="flex min-w-0 items-start gap-2.5"
          data-work-stratum={stratum.depth}
        >
          {/* The depth gutter: a printed number and, under it, the same
            * quantity as a length so the profile of the graph reads without
            * digits. The rail repeats the count beside it and stays hidden. */}
          <span className="flex w-10 shrink-0 flex-col gap-1 pt-1">
            <span
              className="td-value text-right text-2xs text-text-secondary"
              data-cell="numeric"
            >
              {stratum.depth}
            </span>
            <Meter fraction={stratum.components.length / widest} height="row" align="right" />
          </span>
          <ul className="flex min-w-0 flex-1 flex-wrap gap-1.5">
            {stratum.components.map((component) => (
              <li key={component.index} className="min-w-0">
                <ComponentMark
                  component={component}
                  reading={reading}
                  widest={onLongestChain.has(component.index)}
                  selected={selected}
                  onSelect={onSelect}
                />
              </li>
            ))}
          </ul>
        </li>
      ))}
    </ol>
  );
}

/**
 * One condensation component.
 *
 * A single task is one button. A cycle is a bracketed group of buttons wearing
 * the climb hue, labelled with its size, because the members share a stratum
 * and no order among them exists to draw.
 */
function ComponentMark({
  component,
  reading,
  widest,
  selected,
  onSelect,
}: {
  component: WorkDagComponent;
  reading: WorkDagReading;
  widest: boolean;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const cyclic = component.taskIds.length > 1;
  if (!cyclic) {
    const taskId = component.taskIds[0];
    if (taskId === undefined) return null;
    return (
      <TaskMark
        taskId={taskId}
        reading={reading}
        widest={widest}
        selected={selected === taskId}
        onSelect={onSelect}
      />
    );
  }
  return (
    <div
      className="flex min-w-0 flex-wrap items-center gap-1 border border-state-conflicting/60 bg-surface-2 p-1"
      data-work-cycle={component.taskIds.length}
    >
      <span className="td-legend shrink-0 px-1 text-state-conflicting">
        cycle · {component.taskIds.length}
      </span>
      {component.taskIds.map((taskId) => (
        <TaskMark
          key={taskId}
          taskId={taskId}
          reading={reading}
          widest={widest}
          selected={selected === taskId}
          onSelect={onSelect}
        />
      ))}
    </div>
  );
}

function TaskMark({
  taskId,
  reading,
  widest,
  selected,
  onSelect,
}: {
  taskId: string;
  reading: WorkDagReading;
  widest: boolean;
  selected: boolean;
  onSelect: (taskId: string) => void;
}) {
  const node = reading.nodes.get(taskId);
  if (node === undefined) return null;
  const cycleNote = node.cyclic ? ', in a dependency cycle' : '';
  const chainNote = widest ? ', on the deepest chain' : '';
  return (
    <button
      type="button"
      onClick={() => onSelect(taskId)}
      aria-pressed={selected}
      // 44px explicitly rather than a spacing utility: this app's root font
      // size is 14px, so `min-h-11` computes to 38.5px and lands under the
      // target size the accessibility gate measures.
      className={cn(
        'flex min-h-[44px] min-w-0 max-w-[16rem] flex-col justify-center gap-0.5 border px-2 py-1 text-left',
        'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent',
        selected
          ? 'border-accent bg-surface-3'
          : 'border-edge-subtle bg-surface-1 hover:bg-surface-2',
        widest && !selected && 'border-edge-strong',
      )}
      data-work-task={taskId}
      data-work-depth={node.depth}
      data-work-widest={widest ? 'true' : undefined}
    >
      <span className="flex min-w-0 items-center gap-1.5">
        {/* The widest channel is a heavier mark, not only a hue: the deepest
          * chain has to survive a monochrome rendering. */}
        <span
          aria-hidden
          className={cn(
            'shrink-0',
            widest ? 'h-2.5 w-1 bg-accent' : 'size-1.5 bg-edge-strong',
            node.cyclic && 'bg-state-conflicting',
          )}
        />
        <span className="min-w-0 truncate text-2xs text-text-primary">{node.title}</span>
      </span>
      <span className="truncate text-3xs text-text-muted">
        depth {node.depth} · {node.dependencies.length} in · {node.dependents.length} out
        {cycleNote}
        {chainNote}
      </span>
    </button>
  );
}

/** Backward jumps and the cycles they form, stated as observations. */
function ClimbAndCycles({
  reading,
  onSelect,
}: {
  reading: WorkDagReading;
  onSelect: (taskId: string) => void;
}) {
  const climbs = reading.edges.filter((edge) => edge.climb);
  return (
    <Panel
      legend="Backward dependencies"
      actions={
        <StateChip
          kind={climbs.length === 0 ? 'complete_zero_findings' : 'conflicting'}
          detail={`${climbs.length}`}
        />
      }
    >
      {climbs.length === 0 ? (
        <EmptyReading>
          No declared dependency runs backward against the strata. Every edge the snapshot
          returned crosses from a lower stratum to a higher one.
        </EmptyReading>
      ) : (
        <div className="flex min-w-0 flex-col gap-2">
          <p className="text-3xs leading-snug text-text-muted">
            These edges join tasks that already depend on each other, so the condensation
            holds them in one stratum. That is an observation about the plan — a cycle the
            task graph declares — and not an error in this drawing.
          </p>
          <ul className="flex min-w-0 flex-col gap-1">
            {climbs.map((edge) => (
              <li
                key={`${edge.dependency}->${edge.dependent}`}
                className="flex min-w-0 items-center gap-1.5 text-2xs"
              >
                <span aria-hidden className="size-1.5 shrink-0 bg-state-conflicting" />
                <button
                  type="button"
                  onClick={() => onSelect(edge.dependency)}
                  className="min-w-0 truncate font-mono text-text-secondary underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
                >
                  {edge.dependency}
                </button>
                <span aria-hidden className="shrink-0 text-text-muted">
                  gates
                </span>
                <span className="sr-only">gates</span>
                <button
                  type="button"
                  onClick={() => onSelect(edge.dependent)}
                  className="min-w-0 truncate font-mono text-text-secondary underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
                >
                  {edge.dependent}
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Panel>
  );
}

/**
 * Dependencies pointing outside the page.
 *
 * A capped snapshot returns some of the tasks, and an edge whose far end was
 * not returned cannot be layered. Drawing it as satisfied would claim the
 * dependency is met; dropping it would claim it does not exist. It is listed.
 */
function UnresolvedEdges({ reading }: { reading: WorkDagReading }) {
  return (
    <Panel
      legend="Dependencies outside this page"
      actions={
        <StateChip
          kind={reading.unresolved.length === 0 ? 'complete_zero_findings' : 'partial'}
          detail={`${reading.unresolved.length}`}
        />
      }
    >
      {reading.unresolved.length === 0 ? (
        <EmptyReading>
          Every declared dependency names a task this snapshot also returned, so the strata
          above are layered over a complete edge set.
        </EmptyReading>
      ) : (
        <div className="flex min-w-0 flex-col gap-2">
          <p className="text-3xs leading-snug text-text-muted">
            These tasks declare a dependency the snapshot did not return. The edge is real
            and the task at its far end is unread, so neither its stratum nor whether it is
            satisfied can be drawn.
          </p>
          <ul className="flex min-w-0 flex-col gap-1 font-mono text-3xs text-text-secondary">
            {reading.unresolved.map((edge) => (
              <li key={`${edge.dependency}->${edge.dependent}`} className="truncate">
                {edge.dependent} needs {edge.dependency}
              </li>
            ))}
          </ul>
        </div>
      )}
    </Panel>
  );
}
