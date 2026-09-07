import { StateChip } from '../../ui/StateChip.tsx';
import { MeterRow } from '../../ui/instrument.tsx';
import { formatMoment } from '../loom/tracks.ts';
import type { AgentHandoffReading } from './handoff.ts';

/**
 * The handoff frontier: who handed what to whom, with what was known and what
 * was not.
 *
 * Three things this surface refuses to do, each of which it would otherwise do
 * by accident:
 *
 *   - Draw an unread frontier as an empty one. A refusal keeps the daemon's own
 *     state and sentence; there is no row count on the screen at all until a
 *     read has landed.
 *   - Show a handoff as finished work. Every row prints its `unknowns` beside
 *     its `evidence_frontier`, because a handoff that carried three open
 *     questions is a different object from one that carried none, and the
 *     evidence alone reads as the second.
 *   - Report the frontier without its population. `tasksHandedOff` of
 *     `tasksRead` is printed on the caption, so a frontier of two handoffs over
 *     four hundred tasks cannot be read as the whole story of the graph.
 *
 * The rows are a real table with real headers rather than a drawn diagram: the
 * relation here is one arrow between two named actors, and a table states it
 * without needing an accessible equivalent bolted on beside it.
 */
export function AgentHandoffs({ reading }: { reading: AgentHandoffReading }) {
  if (reading.state === 'pending') {
    return <StateChip kind="loading" detail="reading the work-product graph" />;
  }
  if (reading.state === 'refused') {
    return (
      <div className="flex min-w-0 flex-col gap-1" data-agent-handoffs="refused">
        <StateChip kind={reading.chip} detail="handoff frontier" />
        <p className="text-3xs leading-snug text-text-muted">{reading.detail}</p>
        <p className="text-3xs leading-snug text-text-muted">
          No handoff count is shown: the graph was not read, so there is no frontier to be
          empty.
        </p>
      </div>
    );
  }

  const {
    handoffs,
    actors,
    tasksRead,
    tasksHandedOff,
    unknownCount,
    evidenceCount,
    graphVersion,
    observedAtMicros,
    fromTimeline,
  } = reading;

  return (
    <div className="flex min-w-0 flex-col gap-3" data-agent-handoffs={handoffs.length}>
      <p className="td-legend text-text-secondary">
        {handoffs.length === 0
          ? `no handoff on graph version ${graphVersion}`
          : `${handoffs.length} ${handoffs.length === 1 ? 'handoff' : 'handoffs'} · ${tasksHandedOff} of ${tasksRead} ${tasksRead === 1 ? 'task' : 'tasks'} · graph version ${graphVersion}`}
      </p>

      {handoffs.length === 0 ? (
        <p className="text-2xs leading-relaxed text-text-muted" data-agent-handoffs-empty="true">
          The graph answered at version {graphVersion} over {tasksRead}{' '}
          {tasksRead === 1 ? 'task' : 'tasks'} and no task on it carries a handoff record. This
          is the graph saying nothing was handed between actors, not a read that failed to find
          one.
        </p>
      ) : (
        <>
          <p className="text-2xs leading-relaxed text-text-primary">
            {evidenceCount.toLocaleString()}{' '}
            {evidenceCount === 1 ? 'evidence reference was' : 'evidence references were'} carried
            across{' '}
            {unknownCount === 0
              ? 'these handoffs, and none of them declared an open question.'
              : `these handoffs, alongside ${unknownCount.toLocaleString()} declared ${unknownCount === 1 ? 'unknown' : 'unknowns'} — questions the handing-off actor did not answer.`}
          </p>

          {actors.length > 0 ? (
            <figure className="flex min-w-0 flex-col gap-1.5">
              <figcaption className="td-legend">
                actors on the frontier · handed off + received
              </figcaption>
              {actors.map((actor) => (
                <MeterRow
                  key={actor.actor}
                  label={actor.actor}
                  title={actor.actor}
                  fraction={
                    handoffs.length > 0
                      ? (actor.handedOff + actor.received) / (handoffs.length * 2)
                      : null
                  }
                  value={`${actor.handedOff}↦${actor.received}`}
                  figureWidth="wide"
                />
              ))}
              <figcaption className="text-3xs leading-relaxed text-text-muted">
                Each figure is handed-off then received for that actor. The rail is that pair
                summed against every seat on the frontier, so it ranks how much passed through
                an actor and nothing more.
              </figcaption>
            </figure>
          ) : null}

          <div
            role="region"
            aria-label="Handoff records"
            tabIndex={0}
            className="max-h-72 min-w-0 overflow-auto border border-edge-subtle"
          >
            <table className="w-full border-collapse text-2xs">
              <caption className="sr-only">
                Every handoff on this graph version, newest first, with the actor that handed
                the task on, the actor that received it, when it happened, the evidence carried
                across and the questions left open.
              </caption>
              <thead className="sticky top-0 bg-surface-2">
                <tr className="text-left text-text-secondary">
                  <th scope="col" className="px-2 py-1 font-medium">
                    Task
                  </th>
                  <th scope="col" className="px-2 py-1 font-medium">
                    From
                  </th>
                  <th scope="col" className="px-2 py-1 font-medium">
                    To
                  </th>
                  <th scope="col" className="px-2 py-1 text-right font-medium">
                    Handed off
                  </th>
                  <th scope="col" className="px-2 py-1 font-medium">
                    Evidence
                  </th>
                  <th scope="col" className="px-2 py-1 font-medium">
                    Unknowns
                  </th>
                </tr>
              </thead>
              <tbody>
                {handoffs.map((handoff) => (
                  <tr
                    key={handoff.handoffId}
                    className="border-t border-edge-subtle align-top"
                    data-agent-handoff={handoff.handoffId}
                    data-agent-handoff-unknowns={handoff.unknowns.length}
                  >
                    <td className="max-w-0 truncate px-2 py-1 font-mono text-text-secondary">
                      {handoff.taskId}
                    </td>
                    <td className="max-w-0 truncate px-2 py-1 text-text-primary">
                      {handoff.fromActor}
                    </td>
                    <td className="max-w-0 truncate px-2 py-1 text-text-primary">
                      {handoff.toActor}
                    </td>
                    <td
                      className="px-2 py-1 text-right text-text-muted tabular-nums"
                      data-cell="numeric"
                    >
                      {formatMoment(handoff.handedOffAtMicros / 1_000_000)}
                    </td>
                    <td className="px-2 py-1 text-text-secondary">
                      <Refs
                        items={handoff.evidenceFrontier}
                        none="none carried"
                        label={`Evidence carried by handoff ${handoff.handoffId}`}
                      />
                    </td>
                    <td className="px-2 py-1 text-text-secondary">
                      <Refs
                        items={handoff.unknowns}
                        none="none declared"
                        label={`Unknowns declared by handoff ${handoff.handoffId}`}
                      />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}

      <p className="text-3xs leading-relaxed text-text-muted">
        Read from the work-product graph through <span className="td-value">work.views</span>
        {fromTimeline ? ' — the newest version in the returned timeline' : ' in current mode'}
        {Number.isFinite(observedAtMicros)
          ? `, observed ${formatMoment(observedAtMicros / 1_000_000)}`
          : ''}
        . The token-redemption operations named handoff elsewhere in this system open one
        handoff a caller already holds a token for; they cannot enumerate a frontier, so nothing
        here comes from them.
      </p>
    </div>
  );
}

/** A cell's list of references, or the fact that it carried none. Never a blank
 * cell: an empty evidence frontier and an unrendered one look identical, and
 * only one of them is a reading. */
function Refs({
  items,
  none,
  label,
}: {
  items: readonly string[];
  none: string;
  label: string;
}) {
  if (items.length === 0) {
    return <span className="text-3xs text-text-muted">{none}</span>;
  }
  return (
    <ul aria-label={label} className="flex min-w-0 flex-col gap-0.5">
      {items.map((item) => (
        <li key={item} className="truncate font-mono text-3xs" title={item}>
          {item}
        </li>
      ))}
    </ul>
  );
}
