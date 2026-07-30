import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { useEventStreamState, useLiveActivity } from '../../data/sse/useEvents.tsx';
import type { SseConnectionState } from '../../data/sse/connect.ts';

/**
 * The one Work row this build actually reads.
 *
 * The daemon enumerates a `task_activity` family and emits it under that stream
 * name, and the dashboard subscribes to it, so unlike every other row on this
 * page there is a live signal here. What there is not is anywhere to send it: a
 * task-activity frame is an invalidation telling a client to refetch canonical
 * generation-bound projections, and no projection route exists to refetch from.
 * So the row reports the subscription truthfully and refetches nothing.
 *
 * Its own component, and the only one on the page that subscribes, so a burst of
 * frames re-renders this cell rather than the sixteen-row ledger around it.
 *
 * What it counts is what the connection still holds: a bounded decay ring across
 * whichever projects the daemon is reporting, not a total and not a per-project
 * figure. The reading says "received" for that reason — it is a statement about
 * this session's subscription, not about how much work exists.
 */

/**
 * The daemon's family tag for Work task mutations, which is what a pulse is
 * matched on. Not the stream id: those carry a project suffix
 * (`task_activity:<project>`), so an equality test against them never fires.
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
      // Bounded by the connection's decay ring, so this counts what is still in
      // the live window rather than every mutation ever committed. Said that way
      // to avoid reading as a total.
      return observed === 0 ? 'subscribed · none received' : `subscribed · ${observed} received`;
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

  return <StateChip kind={kind} detail={taskActivityReading(link, observed)} />;
}
