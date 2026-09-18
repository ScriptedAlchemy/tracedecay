import { useMemo } from 'react';
import { StateChip } from '../../../ui/StateChip.tsx';
import { Panel } from '../../../ui/instrument.tsx';
import { coverageReading } from '../workModel.ts';
import type { WorkGraphReading } from '../workGraphModel.ts';
import type { WorkProductView } from '../workProductView.ts';
import { type WorkDagReading, workDagReading } from '../workViewsModel.ts';
import { WorkDagBoard } from './WorkDagBoard.tsx';
import { ChannelLedger, EmptyReading } from './WorkViewChannel.tsx';

/**
 * DAG / critical path, the task dependency board over the declared graph.
 *
 * Strata are the longest path over the Tarjan condensation, the same discipline
 * the Code workspace layers imports with: a task sits one stratum below the
 * deepest thing it declares a dependency on, and a dependency cycle is
 * condensed into one mark rather than broken. A backward jump uses the climb
 * hue and the caption must state it is an observation; a declared cycle is a
 * real reading of the plan, not a rendering fault.
 *
 * The board (`WorkDagBoard`) lays those strata out deterministically and draws
 * the three declared relation kinds between the cards. The readings below it
 * are the parts of the graph a drawing cannot carry: the authority's
 * effort-weighted critical path over the WHOLE graph version, which need not
 * agree with the unweighted deepest chain the strata show; the gating edge set
 * the graph declares; the edges that climb inside a cycle; and the
 * dependencies whose far end this page did not return.
 */
export function WorkDagView({
  snapshot,
  graph,
  selected,
  onSelect,
}: {
  snapshot: WorkProductView;
  graph: WorkGraphReading;
  selected: string | null;
  onSelect: (taskId: string) => void;
}) {
  const reading = useMemo(
    () => workDagReading(snapshot.projections, graph),
    [snapshot.projections, graph],
  );
  const coverage = coverageReading(snapshot.coverage);

  return (
    <div className="flex min-w-0 flex-col gap-3" data-work-view="dag">
      <Panel
        legend="Task dependency graph"
        actions={
          <>
            <StateChip kind={coverage.state} detail={coverage.detail} />
            <span className="td-value text-3xs text-text-muted" data-cell="numeric">
              graph v{snapshot.graph_version} · seq {snapshot.event_sequence}
            </span>
          </>
        }
        elevation="well"
      >
        <WorkDagBoard
          snapshot={snapshot}
          reading={reading}
          selected={selected}
          onSelect={onSelect}
        />
      </Panel>

      <CriticalPath reading={reading} />

      <div className="grid min-w-0 gap-3 lg:grid-cols-2">
        <ClimbAndCycles reading={reading} onSelect={onSelect} />
        <div className="flex min-w-0 flex-col gap-3">
          <UnresolvedEdges reading={reading} />
          <GatingEdges reading={reading} />
          <ChannelLedger
            legend="Measurements this projection could not take"
            channels={[
              { measure: 'effort-weighted critical path', channel: reading.effort },
              { measure: 'declared gating edges', channel: reading.gating },
            ]}
          />
        </div>
      </div>
    </div>
  );
}

/**
 * The authority's effort-weighted critical path.
 *
 * Task ids are printed rather than offered as buttons, and that is the reading
 * rather than an omission: this chain comes off the work-product graph and the
 * strata above come off the snapshot page, so a task on it need not be a task
 * this page returned. A button that moved the selection to a task the board
 * does not hold would move it to nothing.
 */
function CriticalPath({ reading }: { reading: WorkDagReading }) {
  const chain = reading.effort;
  return (
    <Panel
      legend="Effort-weighted critical path"
      actions={
        chain.available ? (
          <StateChip
            kind={chain.value.taskIds.length === 0 ? 'complete_zero_findings' : 'ready'}
            detail={`${chain.value.totalEffort} effort`}
          />
        ) : (
          <StateChip kind={chain.state} detail="not weighted" />
        )
      }
    >
      {!chain.available ? (
        <p className="text-3xs leading-snug text-text-muted" data-work-critical-path="absent">
          {chain.detail}
        </p>
      ) : chain.value.taskIds.length === 0 ? (
        <EmptyReading>
          The work-product graph weighted its critical path and the path is empty: this graph
          version declares no chain of work to weigh. That is the authority answering, not a
          measurement that failed.
        </EmptyReading>
      ) : (
        <div
          className="flex min-w-0 flex-col gap-2"
          data-work-critical-path={chain.value.taskIds.length}
          data-work-critical-effort={chain.value.totalEffort}
        >
          <p className="text-3xs leading-snug text-text-muted">
            {chain.value.taskIds.length}{' '}
            {chain.value.taskIds.length === 1 ? 'task' : 'tasks'} carrying{' '}
            {chain.value.totalEffort} declared effort, weighted by the work-product graph over
            its whole graph version. The strata above are the longest path over the edges THIS
            page returned and are unweighted, so the two chains answer different questions and
            need not agree.
          </p>
          <ol className="flex min-w-0 flex-wrap items-center gap-1 font-mono text-3xs text-text-secondary">
            {chain.value.taskIds.map((taskId, index) => (
              <li key={`${taskId}#${index}`} className="flex min-w-0 items-center gap-1">
                {index === 0 ? null : (
                  <span aria-hidden className="shrink-0 text-text-muted">
                    ›
                  </span>
                )}
                <span className="min-w-0 truncate">{taskId}</span>
              </li>
            ))}
          </ol>
        </div>
      )}
    </Panel>
  );
}

/**
 * The gating edge set the work-product graph declares.
 *
 * Declared data, so an empty set is an answer: this graph version gates
 * nothing. It is listed apart from the snapshot's declared edges rather than
 * merged with them, because the two reads cover different populations and a
 * merged count would be a total over a set neither read returned.
 */
function GatingEdges({ reading }: { reading: WorkDagReading }) {
  const gating = reading.gating;
  return (
    <Panel
      legend="Gating edges the graph declares"
      actions={
        gating.available ? (
          <StateChip
            kind={gating.value.length === 0 ? 'complete_zero_findings' : 'ready'}
            detail={`${gating.value.length}`}
          />
        ) : (
          <StateChip kind={gating.state} detail="not read" />
        )
      }
    >
      {!gating.available ? (
        <p className="text-3xs leading-snug text-text-muted">{gating.detail}</p>
      ) : gating.value.length === 0 ? (
        <EmptyReading>
          The work-product graph declares no gating edge at all. Nobody wrote one down, this is
          the authority answering the question, not the question going unasked.
        </EmptyReading>
      ) : (
        <ul
          className="flex min-w-0 flex-col gap-1 font-mono text-3xs text-text-secondary"
          data-work-gating={gating.value.length}
        >
          {gating.value.map((edge, index) => (
            <li key={`${edge.dependency}->${edge.dependent}#${index}`} className="truncate">
              {edge.dependent} needs {edge.dependency}
            </li>
          ))}
        </ul>
      )}
    </Panel>
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
            holds them in one stratum. That is an observation about the plan, a cycle the
            task graph declares, and not an error in this drawing.
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
