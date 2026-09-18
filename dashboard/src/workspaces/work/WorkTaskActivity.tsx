import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { useEventStreamState, useLiveActivity } from '../../data/sse/useEvents.tsx';
import type { LiveActivityPulse, SseConnectionState } from '../../data/sse/connect.ts';
import { useScope, type DashboardScope } from '../../data/scope/store.ts';

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
 * What the pulse buffer holds for the scope being reported.
 *
 * Two figures rather than one, because a project-scoped count cannot answer for
 * a frame that named no project and must not silently drop it either. The
 * stream is one connection shared by the whole dashboard, `/api/events` is not
 * per-project, and inventing a scoped subscription is not this row's business,
 * so the scoping happens here, over what the buffer already holds.
 */
export interface TaskActivityWindow {
  /** Task pulses the window holds that this scope can claim. */
  readonly observed: number;
  /**
   * Task pulses carried without a project attribution.
   *
   * Always zero under the all-projects scope, which claims them: the aggregate
   * is answering for every project, so a frame that named none is still a frame
   * it received. Under a selected project they are counted separately, folding
   * them in would attribute another project's work to this one, and dropping
   * them unmentioned would report an absence of work caused by an absence of
   * attribution.
   */
  readonly unattributed: number;
}

/**
 * Count the buffer for one scope.
 *
 * Pure, and separate from the component, because the defect it fixes is a
 * counting rule rather than a rendering one: the row matched
 * `family === 'task_activity'` and nothing else, so under project B the reading
 * included project A's pulses from the shared buffer and reported them as B's.
 */
export function taskActivityWindow(
  pulses: readonly LiveActivityPulse[],
  scope: DashboardScope,
): TaskActivityWindow {
  let observed = 0;
  let unattributed = 0;
  for (const pulse of pulses) {
    if (pulse.family !== TASK_FAMILY) continue;
    if (scope.kind !== 'project') {
      observed += 1;
    } else if (pulse.projectId === scope.projectId) {
      observed += 1;
    } else if (pulse.projectId === null) {
      unattributed += 1;
    }
    // A pulse attributed to another project is not this scope's to report at
    // all, in either figure.
  }
  return { observed, unattributed };
}

/**
 * What the row can honestly say about the stream.
 *
 * Separated by link state first, because "nothing has arrived" and "nothing
 * could arrive" are different facts and only the second is a failure. A silent
 * live stream is reported as silent, never as zero task activity: this build
 * cannot see whether a producer is mounted, only whether it has received
 * anything.
 */
export function taskActivityReading(
  link: SseConnectionState,
  window: TaskActivityWindow,
): string {
  switch (link) {
    case 'offline':
      return 'subscribed · stream unreachable';
    case 'connecting':
      return 'subscribed · connecting';
    case 'live': {
      // Named as a window because that is what it measures: the shared pulse
      // buffer holds 64 entries across every family, so this is what is still
      // retained, never a count of what the daemon has committed.
      const counted =
        window.observed === 0 ? 'none in live window' : `${window.observed} in live window`;
      // Said only when there is something to say, so the all-projects reading
      // and a cleanly attributed project reading are unchanged.
      return window.unattributed === 0
        ? `subscribed · ${counted}`
        : `subscribed · ${counted} · ${window.unattributed} unattributed`;
    }
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
 * action, and the transition that matters, a live stream going unreachable,
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
  // The buffer is shared by every project on one connection, so the selected
  // scope is what decides which of its frames this row may count.
  const scope = useScope((s) => s.scope);

  return (
    <>
      <StateChip kind={kind} detail={taskActivityReading(link, taskActivityWindow(pulses, scope))} />
      <span role="status" className="sr-only">
        {taskActivityLink(link)}
      </span>
    </>
  );
}

/** One task pulse the ledger can claim for the reported scope. */
export interface TaskActivityRow {
  readonly eventId: string;
  /** Server observation time in microseconds, as the envelope carried it. */
  readonly observedAtMicros: number | null;
  readonly projectId: string | null;
  /** The daemon's bounded detail word, or `null` when the frame named none. */
  readonly detail: string | null;
  /** Under a selected project, a frame that named no project is listed but
   * marked: it is not this project's to claim, and it is not dropped. */
  readonly attribution: 'scoped' | 'unattributed';
}

/**
 * The task pulses the buffer holds for one scope, newest first.
 *
 * The same counting rule as `taskActivityWindow`, kept as rows so the ledger
 * and the chip cannot disagree about which frames this scope may show. The
 * observation time is parsed here because the envelope carries it as a
 * string; an unparsable stamp is `null`, never a guessed instant.
 */
export function taskActivityRows(
  pulses: readonly LiveActivityPulse[],
  scope: DashboardScope,
): readonly TaskActivityRow[] {
  const rows: TaskActivityRow[] = [];
  for (const pulse of pulses) {
    if (pulse.family !== TASK_FAMILY) continue;
    let attribution: TaskActivityRow['attribution'];
    if (scope.kind !== 'project') attribution = 'scoped';
    else if (pulse.projectId === scope.projectId) attribution = 'scoped';
    else if (pulse.projectId === null) attribution = 'unattributed';
    else continue;
    const micros = Number(pulse.observationTime);
    rows.push({
      eventId: pulse.eventId,
      observedAtMicros: Number.isSafeInteger(micros) ? micros : null,
      projectId: pulse.projectId,
      detail: pulse.detail ?? null,
      attribution,
    });
  }
  return rows.reverse();
}

/**
 * The live task activity ledger.
 *
 * Every row is one admitted `task_activity` frame the connection still holds,
 * printed with exactly what the frame carried: its observation instant, the
 * project it named, and the daemon's bounded detail word when the mutation
 * emitted one. The frame does not carry a task identity or title, the
 * daemon coalesces task mutations per project, so those columns are not
 * drawn, and the caption says why. Mounted reads refetch the canonical graph
 * on every frame; this ledger is a window on the stream, not the graph.
 */
export function WorkActivityLedger({ limit = 12 }: { limit?: number }) {
  const { state: link } = useEventStreamState();
  const { pulses } = useLiveActivity();
  const scope = useScope((s) => s.scope);
  const allRows = taskActivityRows(pulses, scope);
  const rows = allRows.slice(0, limit);
  const window = taskActivityWindow(pulses, scope);

  return (
    <Panel
      legend="Live task activity"
      actions={<WorkTaskActivity kind={link === 'live' ? 'ready' : link === 'connecting' ? 'loading' : 'offline'} />}
      bodyClassName="p-0"
      elevation="well"
    >
      <div role="region" aria-label="Live task activity ledger" tabIndex={0} className="min-w-0 overflow-x-auto" data-work-activity-ledger={rows.length}>
        <table className="w-full min-w-0 border-collapse text-2xs">
          <caption className="sr-only">
            Admitted task-activity frames still held by the live connection, newest first, with
            the observation instant, project, and detail word each frame carried. The stream
            coalesces task mutations per project and carries no task identity or title, so
            neither is a column; canonical Work reads refetch on every frame.
          </caption>
          <thead>
            <tr className="border-b border-edge text-text-muted">
              <th scope="col" className="px-2 py-1 text-left font-medium">Observed (UTC)</th>
              <th scope="col" className="px-2 py-1 text-left font-medium">Project</th>
              <th scope="col" className="px-2 py-1 text-left font-medium">Event</th>
              <th scope="col" className="px-2 py-1 text-left font-medium max-md:hidden">Frame</th>
            </tr>
          </thead>
          <tbody>
            {rows.length === 0 ? (
              <tr>
                <td colSpan={4} className="px-2 py-2 text-text-muted" data-work-activity-empty={link}>
                  {link === 'live'
                    ? 'No task frame in the live window. The buffer holds 64 frames across every family, so this is what is retained, not a count of committed mutations.'
                    : link === 'connecting'
                      ? 'The stream is connecting; no frame has arrived yet.'
                      : 'The stream is unreachable; nothing could arrive. Canonical reads on this page are captured, not live.'}
                </td>
              </tr>
            ) : (
              rows.map((row) => (
                <tr key={row.eventId} className="border-b border-edge-subtle last:border-b-0" data-work-activity-row={row.attribution}>
                  <td className="td-value whitespace-nowrap px-2 py-1 text-text-secondary">
                    {row.observedAtMicros === null ? 'unparsed stamp' : formatMicrosUtc(row.observedAtMicros)}
                  </td>
                  <td className="td-value px-2 py-1 text-text-secondary">
                    {row.projectId ?? 'unattributed'}
                    {row.attribution === 'unattributed' ? (
                      <span className="td-legend ml-2 text-state-partial">not this scope's</span>
                    ) : null}
                  </td>
                  <td className="px-2 py-1 text-text-secondary">
                    {row.detail === null ? (
                      <span className="text-text-muted">task mutation committed · kind not carried by the frame</span>
                    ) : (
                      <span className="td-value">{row.detail}</span>
                    )}
                  </td>
                  <td className="td-value px-2 py-1 text-3xs text-text-muted max-md:hidden">{row.eventId}</td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>
      {window.unattributed > 0 || rows.length < allRows.length ? (
        <p className="border-t border-edge-subtle px-2 py-1 text-3xs text-text-muted">
          {allRows.length} task frames in the live window
          {rows.length < allRows.length ? ` · newest ${rows.length} shown` : ''}
          {window.unattributed > 0 ? ` · ${window.unattributed} unattributed` : ''}
        </p>
      ) : null}
    </Panel>
  );
}
