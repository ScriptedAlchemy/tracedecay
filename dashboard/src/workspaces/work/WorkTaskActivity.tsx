import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { useEventStreamState, useLiveActivity } from '../../data/sse/useEvents.tsx';
import type { SseConnectionState } from '../../data/sse/connect.ts';

/**
 * The one Work row this build actually reads.
 *
 * The daemon enumerates a `task_activity` family and emits it under that stream
 * name, and the dashboard subscribes to it, so unlike every other row on this
 * page there is a live signal here. Each frame also invalidates the snapshot
 * and delta query prefixes for the exact project in its event scope; mounted
 * Work reads refetch from the canonical projection routes instead of treating
 * this pulse window as projection data.
 *
 * Its own component, and the only one on the page that subscribes, so a burst of
 * frames re-renders this cell rather than the sixteen-row ledger around it.
 *
 * What it counts is only what the connection still holds. That store is a
 * 64-entry buffer shared by every event family, so unrelated hook and tool-call
 * traffic evicts task pulses and the figure falls while the stream stays live.
 * It is therefore reported as a live window rather than as "received": a count
 * that can decay to zero during active work must not be worded as a total, or
 * this row would report an absence of work on the strength of other work.
 */

/**
 * The daemon's family tag for Work task mutations, which is what a pulse is
 * matched on. Not the stream: every activity family shares the single
 * `dashboard_activity` stream, so matching on that would count all five.
 */
const TASK_FAMILY = 'task_activity';

/**
 * What the row can honestly say about the stream.
 *
 * Separated by link state first, because "nothing has arrived" and "nothing
 * could arrive" are different facts and only the second is a failure. A silent
 * live stream is reported as silent, never as zero task activity: this build
 * cannot see whether a producer is mounted, only whether it has received
 * anything.
 */
export function taskActivityReading(link: SseConnectionState, observed: number): string {
  switch (link) {
    case 'offline':
      return 'subscribed · stream unreachable';
    case 'connecting':
      return 'subscribed · connecting';
    case 'live':
      // Named as a window because that is what it measures: the shared pulse
      // buffer holds 64 entries across every family, so this is what is still
      // retained, never a count of what the daemon has committed.
      return observed === 0
        ? 'subscribed · none in live window'
        : `subscribed · ${observed} in live window`;
    default: {
      const unhandled: never = link;
      return unhandled;
    }
  }
}

/**
 * The link alone, without the count.
 *
 * Announced rather than only drawn. The chip's reading changes with no user
 * action, and the transition that matters — a live stream going unreachable —
 * is the difference between "quiet" and "blind" this component exists to state.
 * A sighted reader sees that flip; without a status region nobody else does.
 *
 * The count is deliberately left out of it. Politely announcing every accepted
 * frame would read a new number over the top of whatever the user was doing,
 * which is why the live region carries the link and the chip carries the rest.
 */
export function taskActivityLink(link: SseConnectionState): string {
  switch (link) {
    case 'offline':
      return 'Work task activity: stream unreachable';
    case 'connecting':
      return 'Work task activity: connecting';
    case 'live':
      return 'Work task activity: subscribed and live';
    default: {
      const unhandled: never = link;
      return unhandled;
    }
  }
}

export function WorkTaskActivity({ kind }: { kind: DomainStateKind }) {
  const { state: link } = useEventStreamState();
  const { pulses } = useLiveActivity();
  const observed = pulses.filter((pulse) => pulse.family === TASK_FAMILY).length;

  return (
    <>
      <StateChip kind={kind} detail={taskActivityReading(link, observed)} />
      <span role="status" className="sr-only">
        {taskActivityLink(link)}
      </span>
    </>
  );
}
