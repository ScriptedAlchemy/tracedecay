import type { WorkProjectionSnapshotV1 } from '../../../contracts/index.ts';
import { type DomainStateKind, StateChip } from '../../../ui/StateChip.tsx';
import { MeterRow, Panel } from '../../../ui/instrument.tsx';
import { cn } from '../../../ui/cn.ts';
import { coverageReading } from '../workModel.ts';
import {
  type WorkCausalEdge,
  type WorkCausalReading,
  type WorkCausalReadingKind,
  causalReadingLabel,
  causalReadingState,
  workCausalReading,
} from '../workViewsModel.ts';
import { ChannelLedger, EmptyReading, ViewCaption } from './WorkViewChannel.tsx';

/**
 * Causal — the disagreement field over the declared dependency graph.
 *
 * The field holds what the plan declares against what the run left behind, and
 * the loud reading is an edge whose dependent carries terminal evidence while
 * the dependency it names carries none. That dependency did not gate the work,
 * or it is not the dependency the plan says it is; either way the coupling
 * that actually held is not written down. Plan 11c calls that hidden coupling
 * in the plan itself, and it is the reading this projection exists to surface.
 *
 * Half the field is missing. Observed execution order needs attempt
 * timestamps, and the field's other half — an edge that executed but was never
 * declared — cannot be found at all without an order to find it against. Both
 * are drawn as the absences they are, because a page that quietly omitted them
 * would read as a survey for undeclared coupling that came back clean.
 *
 * Nothing here is scored. Three of the five readings order nothing: both ends
 * finished with no clock between them, neither end has finished, or the far
 * end was never returned. One percentage over the set would report every one
 * of those as agreement, so the bands are counted and drawn apart instead.
 *
 * Accessibility. The field is a list of readings and a table of edges with a
 * real button at every task the snapshot returned, so the visualization IS the
 * accessible structure: it takes Tab in reading order and needs no parallel
 * text twin. Hue marks repeat a word printed beside them and stay out of the
 * accessibility tree.
 */

/** Spelled out per reading rather than derived from the state: Tailwind builds
 * utilities by scanning literal source text, so a computed class name would
 * never exist. The hues are the taxonomy's own, via `causalReadingState`. */
const READING_FILL: Record<WorkCausalReadingKind, string> = {
  dependent_ahead: 'bg-state-conflicting',
  consistent: 'bg-state-ready',
  order_unread: 'bg-state-unknown',
  unobserved: 'bg-state-partial',
  unresolved: 'bg-state-offline',
};

/**
 * The two bands of the distribution.
 *
 * The split is the honest one: an edge whose two ends carry different evidence
 * can be read as an order, and an edge whose ends carry the same evidence — or
 * whose far end is off the page — cannot be read at all. `order_unread` sits
 * below the rule with the other unknowns, never beside `consistent`.
 */
const READING_BANDS: readonly {
  readonly legend: string;
  readonly kinds: readonly WorkCausalReadingKind[];
  readonly note: string;
}[] = [
  {
    legend: 'What the evidence orders',
    kinds: ['dependent_ahead', 'consistent'],
    note: 'One end carries terminal runtime evidence and the other does not, so these edges read as an order. Consistent means consistent so far — the dependent has not finished, and nothing has been proved about it.',
  },
  {
    legend: 'What nothing here orders',
    kinds: ['order_unread', 'unobserved', 'unresolved'],
    note: 'Both ends finished with no clock between them, neither end has finished, or the far end is outside this page. None of these is agreement.',
  },
];

export function WorkCausalView({
  snapshot,
  selected,
  onSelect,
}: {
  snapshot: WorkProjectionSnapshotV1;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const reading = workCausalReading(snapshot.projections);
  const coverage = coverageReading(snapshot.coverage);
  // Titles come off the snapshot rather than the reading: the causal reading
  // carries identities only, and a task with no title here is a task the page
  // did not return.
  const titles: ReadonlyMap<string, string> = new Map(
    snapshot.projections.map((projection) => [projection.task_id, projection.title]),
  );

  return (
    <div
      className="flex min-w-0 flex-col gap-3"
      data-work-view="causal"
      data-work-declared={reading.declared}
    >
      <Panel
        legend="Declared against observed"
        actions={<StateChip kind={coverage.state} detail={coverage.detail} />}
        elevation="well"
      >
        <div className="flex min-w-0 flex-col gap-3">
          <ViewCaption
            population={`${plural(reading.declared, 'declared edge')} · ${plural(snapshot.projections.length, 'task')}`}
            note={
              reading.declared === 0
                ? undefined
                : `${reading.disagreements.length} read as disagreement · ${unordered(reading)} nothing here can order`
            }
          />

          {snapshot.projections.length === 0 ? (
            <EmptyReading>
              The snapshot returned no tasks, so there is no declared order to overlay. This is
              the daemon reporting an empty board, not a projection that failed to draw.
            </EmptyReading>
          ) : reading.declared === 0 ? (
            <EmptyReading>
              No task this page returned declares a dependency at all, so the field has nothing
              to overlay. The distribution is not drawn: five bars reading zero would look like
              a measurement of the plan rather than the absence of one.
            </EmptyReading>
          ) : (
            <ReadingDistribution reading={reading} />
          )}
        </div>
      </Panel>

      <Disagreements
        reading={reading}
        titles={titles}
        selected={selected}
        onSelect={onSelect}
      />

      <div className="grid min-w-0 gap-3 lg:grid-cols-2">
        <UndeclaredCoupling />
        <ChannelLedger
          legend="Measurements this projection could not take"
          channels={[
            { measure: 'observed execution order', channel: reading.observedOrder },
            {
              measure: 'executed-before-but-undeclared edges',
              channel: reading.undeclared,
            },
          ]}
        />
      </div>

      <DeclaredEdges
        reading={reading}
        titles={titles}
        selected={selected}
        onSelect={onSelect}
      />
    </div>
  );
}

/** Edges no evidence on this page can put in an order. */
function unordered(reading: WorkCausalReading): number {
  return reading.counts.order_unread + reading.counts.unobserved + reading.counts.unresolved;
}

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? '' : 's'}`;
}

function edgeKey(edge: WorkCausalEdge): string {
  return `${edge.dependency}->${edge.dependent}`;
}

/**
 * All five readings, always, in two bands.
 *
 * A reading of zero is a reading and stays on the page. What does not appear
 * is a total: the closing sentence states how much of the set nothing orders,
 * which is the number an agreement score would have hidden.
 */
function ReadingDistribution({ reading }: { reading: WorkCausalReading }) {
  return (
    <div className="flex min-w-0 flex-col gap-3" data-work-distribution={reading.declared}>
      {READING_BANDS.map((band) => (
        <section
          key={band.legend}
          aria-label={band.legend}
          className="flex min-w-0 flex-col gap-1.5"
        >
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="td-legend truncate text-text-secondary">{band.legend}</h3>
            <span aria-hidden className="td-rule" />
          </div>
          <ul className="flex min-w-0 flex-col gap-1">
            {band.kinds.map((kind) => (
              <li
                key={kind}
                className="min-w-0"
                data-work-causal-reading={kind}
                data-work-causal-count={reading.counts[kind]}
              >
                <MeterRow
                  leading={
                    <StateChip kind={causalReadingState(kind)} className="shrink-0" />
                  }
                  label={causalReadingLabel(kind)}
                  title={causalReadingLabel(kind)}
                  value={reading.counts[kind]}
                  fraction={reading.counts[kind] / reading.declared}
                  tone={READING_FILL[kind]}
                />
              </li>
            ))}
          </ul>
          <p className="text-3xs leading-snug text-text-muted">{band.note}</p>
        </section>
      ))}
      <p className="text-3xs leading-snug text-text-muted">
        No agreement score is printed. {unordered(reading)} of {reading.declared} declared edges
        sit in the lower band, and one percentage over the whole set would report every one of
        them as agreement.
      </p>
    </div>
  );
}

/**
 * The loud reading, listed exhaustively.
 *
 * A dependent carrying terminal evidence while its declared dependency carries
 * none is the one disagreement this overlay can state without a clock, and it
 * is the one worth stating: the declared edge did not gate the work.
 */
function Disagreements({
  reading,
  titles,
  selected,
  onSelect,
}: {
  reading: WorkCausalReading;
  titles: ReadonlyMap<string, string>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  // Unresolved edges were not read, so a clean sweep over the rest is partial
  // rather than complete.
  const state: DomainStateKind =
    reading.disagreements.length > 0
      ? 'conflicting'
      : reading.counts.unresolved > 0
        ? 'partial'
        : 'complete_zero_findings';

  return (
    <Panel
      legend="Dependents that finished first"
      actions={
        <StateChip
          kind={state}
          detail={`${reading.disagreements.length} of ${reading.declared}`}
        />
      }
    >
      {reading.disagreements.length === 0 ? (
        <EmptyReading>
          {reading.declared === 0
            ? 'The tasks on this page declare no dependencies, so there is no declared edge for the evidence to contradict.'
            : `No declared dependency on this page has a dependent carrying terminal evidence while the dependency itself carries none.${
                reading.counts.unresolved > 0
                  ? ` ${reading.counts.unresolved} of them could not be read at all, because the task at the far end was not returned.`
                  : ''
              } This is one reading coming back empty over ${plural(reading.declared, 'declared edge')} — not a finding that the plan and the run agree.`}
        </EmptyReading>
      ) : (
        <div className="flex min-w-0 flex-col gap-2">
          <p className="text-3xs leading-snug text-text-muted">
            Each pair below declares a dependency the evidence does not support: the dependent
            finished, and the task it waits on has no terminal evidence at all. The dependency
            did not gate this work, or it is not the dependency the plan names. Either way the
            coupling that held is not the coupling that is written down.
          </p>
          <ul
            className="flex min-w-0 flex-col gap-1.5"
            data-work-disagreements={reading.disagreements.length}
          >
            {reading.disagreements.map((edge, index) => (
              <li key={`${edgeKey(edge)}#${index}`} className="min-w-0">
                <div
                  className="flex min-w-0 flex-col gap-2 border border-edge-subtle bg-surface-2 p-2 sm:flex-row sm:items-stretch sm:gap-3"
                  data-work-edge={edgeKey(edge)}
                >
                  <div className="flex min-w-0 flex-1 flex-col gap-1">
                    <span className="td-legend text-text-muted">
                      Declared dependency · no terminal evidence
                    </span>
                    <TaskMark
                      taskId={edge.dependency}
                      title={titles.get(edge.dependency)}
                      selected={selected === edge.dependency}
                      onSelect={onSelect}
                    />
                  </div>
                  {/* The relation is printed in the two labels either side, so
                    * the rule between them carries no meaning of its own. */}
                  <span
                    aria-hidden
                    className="hidden w-px shrink-0 self-stretch bg-state-conflicting sm:block"
                  />
                  <div className="flex min-w-0 flex-1 flex-col gap-1">
                    <span className="td-legend text-text-muted">
                      Dependent · terminal evidence
                    </span>
                    <TaskMark
                      taskId={edge.dependent}
                      title={titles.get(edge.dependent)}
                      selected={selected === edge.dependent}
                      onSelect={onSelect}
                    />
                  </div>
                </div>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Panel>
  );
}

/**
 * The half of the field that is not drawn.
 *
 * An undeclared edge is one the plan never wrote down, and it can only be
 * found by holding the plan against an observed order. With no order the
 * search cannot be run at all — a different statement from running it and
 * finding nothing, and the two must never render alike.
 */
function UndeclaredCoupling() {
  return (
    <Panel
      legend="Coupling this view did not survey"
      actions={<StateChip kind="unsupported_schema" detail="not surveyed" />}
    >
      <div
        className="flex min-w-0 flex-col gap-2 text-3xs leading-snug text-text-muted"
        data-work-undeclared="not-surveyed"
      >
        <p>
          The other half of a disagreement field is the edge that executed but was never
          declared: one task in fact waited on another and the plan says nothing about it.
          Finding one needs an observed execution order to hold the plan against, and this read
          carries no timestamp anywhere.
        </p>
        <p>
          So this page has not searched for undeclared coupling and come back empty. It could
          not search. Nothing about hidden coupling follows from the fact that none is listed
          here — the measurement was not taken.
        </p>
      </div>
    </Panel>
  );
}

/**
 * The population the two panels above are drawn out of.
 *
 * Every declared edge with its reading, in the reading's own order. The far
 * end of an unresolved edge is printed rather than offered as a button: it
 * names a task this page does not hold, and selecting it would move the board
 * to nothing.
 */
function DeclaredEdges({
  reading,
  titles,
  selected,
  onSelect,
}: {
  reading: WorkCausalReading;
  titles: ReadonlyMap<string, string>;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  return (
    <Panel
      legend="Every declared edge"
      actions={
        <StateChip
          kind={reading.declared === 0 ? 'complete_zero_findings' : 'ready'}
          detail={`${reading.declared}`}
        />
      }
      bodyClassName={reading.declared === 0 ? undefined : 'p-0'}
    >
      {reading.declared === 0 ? (
        <EmptyReading>
          No task on this page declares a dependency, so there is no edge population to list.
        </EmptyReading>
      ) : (
        <div
          role="region"
          aria-label="Declared dependency edges"
          tabIndex={0}
          className="min-w-0 overflow-x-auto"
        >
          <table className="w-full min-w-0 border-collapse text-2xs">
            <caption className="sr-only">
              Every dependency the returned tasks declare, with the task it names, the task that
              declares it, and what the terminal runtime evidence at the two ends reads as.
            </caption>
            <thead>
              <tr className="border-b border-edge text-text-muted">
                <th scope="col" className="px-2 py-1 text-left font-medium">
                  Dependency
                </th>
                <th scope="col" className="px-2 py-1 text-left font-medium">
                  Dependent
                </th>
                <th scope="col" className="px-2 py-1 text-left font-medium">
                  Reading
                </th>
              </tr>
            </thead>
            <tbody>
              {reading.edges.map((edge, index) => (
                <tr
                  key={`${edgeKey(edge)}#${index}`}
                  data-work-edge={edgeKey(edge)}
                  data-work-causal-reading={edge.kind}
                >
                  <th
                    scope="row"
                    className="min-w-0 px-2 py-1 text-left align-top font-normal"
                  >
                    {edge.kind === 'unresolved' ? (
                      <span className="flex min-h-[44px] min-w-0 items-center truncate font-mono text-3xs text-text-muted">
                        {edge.dependency}
                      </span>
                    ) : (
                      <TaskMark
                        taskId={edge.dependency}
                        title={titles.get(edge.dependency)}
                        selected={selected === edge.dependency}
                        onSelect={onSelect}
                      />
                    )}
                  </th>
                  <td className="min-w-0 px-2 py-1 align-top">
                    <TaskMark
                      taskId={edge.dependent}
                      title={titles.get(edge.dependent)}
                      selected={selected === edge.dependent}
                      onSelect={onSelect}
                    />
                  </td>
                  <td className="px-2 py-1 align-top">
                    <span className="flex min-h-[44px] min-w-0 items-center gap-1.5 text-text-secondary">
                      {/* The hue repeats a word set beside it, so the reading
                        * survives a monochrome rendering. */}
                      <span
                        aria-hidden
                        className={cn('size-1.5 shrink-0', READING_FILL[edge.kind])}
                      />
                      <span className="min-w-0">{causalReadingLabel(edge.kind)}</span>
                    </span>
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

/** One endpoint of an edge. Selecting it is how a reader reaches the evidence
 * the reading was taken from, so every task the snapshot returned is a real
 * button rather than a label. */
function TaskMark({
  taskId,
  title,
  selected,
  onSelect,
}: {
  taskId: string;
  title: string | undefined;
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
      className={cn(
        'flex min-h-[44px] w-full min-w-0 flex-col justify-center gap-0.5 border px-2 py-1 text-left',
        'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent',
        selected
          ? 'border-accent bg-surface-3'
          : 'border-edge-subtle bg-surface-1 hover:bg-surface-2',
      )}
      data-work-task={taskId}
    >
      {title === undefined ? null : (
        <span className="min-w-0 truncate text-2xs text-text-primary">{title}</span>
      )}
      <span className="min-w-0 truncate font-mono text-3xs text-text-muted">{taskId}</span>
    </button>
  );
}
