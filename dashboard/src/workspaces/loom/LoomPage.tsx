import { useMemo, useState } from 'react';
import { Waypoints } from 'lucide-react';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { StateChip } from '../../ui/StateChip';
import {
  Legend,
  Panel,
  Readout,
  ReadoutBar,
  WorkspaceHeader,
} from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn';
import { formatCount } from '../../ui/format.ts';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { MARK_PITCH_PX, PLOT_HEIGHT, WeaveCanvas } from './WeaveCanvas.tsx';
import { ThreadChain } from './ThreadChain.tsx';
import { formatDuration, formatMoment } from './tracks.ts';
import {
  composeWeave,
  extentOf,
  threadsFrom,
  WEFT_SOURCES,
  type PlacedThread,
} from './weave.ts';
import { LoomSessionsPayloadSchema, LoomTimelinePayloadSchema } from './contracts.ts';

/**
 * Loom — time and causality.
 *
 * The plan asks this surface for "interactive temporal and causal traces
 * linking prompts, reasoning, tools, subagents, code changes, branches,
 * commits, PRs, and outcomes". The daemon serves the first half of that
 * sentence and none of the second, so this page draws the first half properly
 * and prints the second half as an itemised absence. The reasoning, in full,
 * is in `contracts.ts` and `weave.ts`; the short version:
 *
 *   - Threads are real. Every mark is one session at its real start time, as
 *     thick as its real message count, in its host's column.
 *   - Extent is honest per thread. Most sessions have no served end, so most
 *     threads are open, and they LOOK open.
 *   - Crossings are absent. Not thin, not faint — absent, with the store
 *     location of each missing relation named beside it.
 *
 * Selecting a thread isolates it and pulls its chain — prompt, turns, tools —
 * from the LCM session endpoint into the rail. That chain stops at tools,
 * because that is where the wire stops.
 *
 * Note the endpoint choice: this page does NOT read
 * `/api/plugins/hermes-lcm/overview`, which is the obvious source and is
 * returning HTTP 500 on the real profile (a payload-health probe inside the
 * handler fails and takes the whole response with it). The sessions rollup
 * under `/api/plugins/savings/sessions` serves strictly more of what the weave
 * needs — start times, message counts, subagent flags, model attribution —
 * and serves it.
 */
export function LoomPage() {
  const sessions = useLegacy(
    ['loom', 'sessions'],
    '/api/plugins/savings/sessions?limit=200',
    LoomSessionsPayloadSchema,
  );
  const timeline = useLegacy(
    ['loom', 'timeline'],
    '/api/plugins/hermes-lcm/timeline',
    LoomTimelinePayloadSchema,
  );
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const busiestDay = useMemo(() => {
    const buckets =
      timeline.data?.outcome === 'ok' ? (timeline.data.data.buckets ?? []) : [];
    return buckets.reduce<{ bucket: string; count: number } | null>(
      (max, bucket) => (max == null || bucket.count > max.count ? bucket : max),
      null,
    );
  }, [timeline.data]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <WorkspaceHeader
        path="/loom"
        title="Loom"
        note="sessions as threads on a measured time axis · crossings unserved"
      />
      <LegacyBoundary title="Loom" pending={sessions.isPending} result={sessions.data}>
        {(data) => {
          const rows = data.sessions ?? [];

          if (data.available === false) {
            return (
              <div className="flex min-h-0 flex-1 items-center justify-center p-8">
                <div className="flex max-w-sm flex-col items-center gap-3 text-center">
                  <StateChip kind="unknown" detail="session store not readable" />
                  <p className="text-xs leading-relaxed text-text-muted">
                    The daemon answered but reported its session store
                    unavailable, so there is no thread to place on the axis.{' '}
                    <span className="text-text-secondary">
                      This is the store saying so, not an empty result.
                    </span>
                  </p>
                </div>
              </div>
            );
          }

          // Packing needs to know what OVERLAPS ON SCREEN, which is a question
          // about scale, not about the data: at a week per screen two sessions
          // an hour apart are the same mark. So the extent is measured first,
          // converted into the seconds one mark's height covers, and only then
          // is the weave composed. Composing with a zero gap instead left every
          // non-overlapping thread in sub-column zero, which stacked a whole
          // host's threads on one centre line and spent none of the column.
          const extent = extentOf(threadsFrom(rows).threads);
          const minGap = extent
            ? ((extent.end - extent.start) / PLOT_HEIGHT) * MARK_PITCH_PX
            : 0;
          const weave = composeWeave(rows, minGap);
          const selected =
            weave.threads.find((thread) => thread.id === selectedId) ?? null;
          const measuredEnds = weave.threads.length - weave.openEndedCount;
          const messages = weave.threads.reduce(
            (sum, thread) => sum + thread.messages,
            0,
          );

          return (
            <div className="flex min-h-0 flex-1 flex-col">
              <ReadoutBar
                label="Weave readings"
                elevation="raised"
                items={[
                  {
                    label: 'threads',
                    value: weave.threads.length.toLocaleString(),
                    note: data.total ? `of ${formatCount(data.total)} in store` : undefined,
                  },
                  { label: 'hosts', value: weave.hosts.length },
                  { label: 'messages', value: formatCount(messages) },
                  {
                    label: 'measured extent',
                    value: `${measuredEnds}/${weave.threads.length}`,
                    note: 'sessions with a served end',
                    fraction:
                      weave.threads.length > 0
                        ? measuredEnds / weave.threads.length
                        : null,
                  },
                  {
                    label: 'window',
                    value: weave.extent
                      ? formatDuration(weave.extent.end - weave.extent.start)
                      : '—',
                    note: weave.extent ? formatMoment(weave.extent.end) : undefined,
                  },
                  {
                    label: 'busiest day',
                    value: busiestDay ? formatCount(busiestDay.count) : '—',
                    note: busiestDay ? busiestDay.bucket : 'timeline unread',
                  },
                ]}
              />

              <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-3 [scrollbar-gutter:stable] xl:flex-row">
                <div className="flex min-w-0 flex-1 flex-col gap-2">
                  {weave.threads.length === 0 ? (
                    <EmptyWeave undated={weave.undated} rows={rows.length} />
                  ) : (
                    <>
                      <WeaveCanvas
                        weave={weave}
                        selectedId={selectedId}
                        onSelect={setSelectedId}
                        ariaLabel={weaveDescription(weave)}
                      />
                      <WeaveAxis weave={weave} />
                      <ThreadTable
                        threads={weave.threads}
                        selectedId={selectedId}
                        onSelect={setSelectedId}
                      />
                    </>
                  )}
                </div>

                <aside className="flex w-full shrink-0 flex-col gap-3 xl:w-[22rem]">
                  <Panel legend="Causal crossings">
                    <div className="flex flex-col gap-2">
                      <p className="text-2xs leading-relaxed text-text-muted">
                        The weave has warp and no weft. Every crossing this
                        surface is meant to draw is a relation the dashboard API
                        does not serve, so none is drawn. Where TraceDecay does
                        record the relation, the store is named.
                      </p>
                      {WEFT_SOURCES.map((source) => (
                        <div key={source.id} className="flex flex-col gap-1">
                          <span className="td-legend text-text-secondary">
                            {source.label}
                          </span>
                          <StateChip kind="unsupported" detail={source.detail} />
                          {source.store !== '—' ? (
                            <span className="td-value truncate text-3xs text-text-muted">
                              {source.store}
                            </span>
                          ) : null}
                        </div>
                      ))}
                    </div>
                  </Panel>

                  <ThreadChain thread={selected} />
                </aside>
              </div>
            </div>
          );
        }}
      </LegacyBoundary>
    </div>
  );
}

/** The axis, printed. A reader cannot infer from the picture that width is a
 * log of message count, or that a dashed tail is an absence rather than a
 * short session, so both are stated in the same words the code uses. */
function WeaveAxis({ weave }: { weave: ReturnType<typeof composeWeave> }) {
  const busiest = weave.hosts.reduce((max, host) => Math.max(max, host.count), 0);
  return (
    <div className="flex flex-col gap-1.5">
      <Legend>time down · host across · width = messages</Legend>
      <div className="flex flex-wrap border-y border-edge-subtle bg-surface-1">
        {weave.hosts.map((host) => (
          <div
            key={host.id}
            className="min-w-0 flex-1 basis-28 border-l border-edge-subtle px-2.5 py-1.5 first:border-l-0"
          >
            <Readout
              label={host.label}
              value={host.count}
              unit={host.count === 1 ? 'thread' : 'threads'}
              note={`${formatCount(host.messages)} messages`}
              fraction={busiest > 0 ? host.count / busiest : null}
              size="sm"
            />
          </div>
        ))}
      </div>
      <p className="text-2xs leading-relaxed text-text-muted">
        Each thread is one session: vertical position = when it started (exact,
        on the printed axis), column = the host that ran it, width = its message
        count on a log scale. A thread drawn solid to a lower edge has a served
        end time; a thread ending in a short dashed stub does not —{' '}
        <span className="text-text-secondary">
          {weave.openEndedCount} of {weave.threads.length} sessions have no
          served end, so the stub marks unmeasured extent and its length means
          nothing.
        </span>{' '}
        {weave.hollowCount > 0
          ? `${weave.hollowCount} drawn hollow ${weave.hollowCount === 1 ? 'is a session the store reports' : 'are sessions the store reports'} at zero messages — a reading, not a gap. `
          : ''}
        {weave.undated > 0
          ? `${weave.undated} ${weave.undated === 1 ? 'row' : 'rows'} carried no usable start time and ${weave.undated === 1 ? 'is' : 'are'} not on the field at all. `
          : ''}
        Sub-column offset inside a host is packing so threads do not overlap; it
        encodes nothing.
      </p>
    </div>
  );
}

/** The canvas's accessible equivalent (plan 11a archetype 3): the same threads,
 * the same selection, as real rows in a real table. */
function ThreadTable({
  threads,
  selectedId,
  onSelect,
}: {
  threads: readonly PlacedThread[];
  selectedId: string | null;
  onSelect: (id: string | null) => void;
}) {
  return (
    <section aria-label="Threads" className="flex min-w-0 flex-col">
      <Legend>threads · earliest first</Legend>
      <div className="mt-1.5 max-h-72 overflow-auto border border-edge-subtle">
        <table className="w-full border-collapse text-2xs">
          <caption className="sr-only">
            Every session drawn on the weave, in start order, with its host,
            message count and whether the store served an end time.
          </caption>
          <thead className="sticky top-0 bg-surface-2">
            <tr className="text-left text-text-secondary">
              <th scope="col" className="px-2 py-1 font-medium">Session</th>
              <th scope="col" className="px-2 py-1 font-medium">Host</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Started</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Messages</th>
              <th scope="col" className="px-2 py-1 font-medium">Extent</th>
            </tr>
          </thead>
          <tbody>
            {threads.map((thread) => (
              <tr
                key={thread.id}
                className={cn(
                  'border-t border-edge-subtle',
                  selectedId === thread.id && 'bg-accent/10',
                )}
              >
                <td className="max-w-0 px-2 py-1">
                  <button
                    type="button"
                    onClick={() => onSelect(selectedId === thread.id ? null : thread.id)}
                    aria-pressed={selectedId === thread.id}
                    className="flex w-full min-w-0 items-center gap-1.5 text-left"
                  >
                    <span
                      aria-hidden
                      style={kindColorVars(thread.host)}
                      className="size-1.5 shrink-0 bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
                    />
                    <span className="truncate text-text-primary">{thread.label}</span>
                    {thread.isSubagent ? (
                      <span className="td-legend shrink-0 text-text-muted">sub</span>
                    ) : null}
                  </button>
                </td>
                <td className="px-2 py-1 text-text-secondary">{thread.host}</td>
                <td
                  className="px-2 py-1 text-right text-text-muted tabular-nums"
                  data-cell="numeric"
                >
                  {formatMoment(thread.start)}
                </td>
                <td
                  className="px-2 py-1 text-right text-text-secondary tabular-nums"
                  data-cell="numeric"
                >
                  {thread.messages.toLocaleString()}
                </td>
                <td className="px-2 py-1 text-text-muted">
                  {thread.end != null
                    ? formatDuration(thread.end - thread.start)
                    : 'not served'}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function weaveDescription(weave: ReturnType<typeof composeWeave>): string {
  const hosts = weave.hosts
    .map((host) => `${host.count} on ${host.label}`)
    .join(', ');
  return `Weave: ${weave.threads.length} sessions as vertical threads, time running downward, one column per host (${hosts || 'none'}). ${weave.openEndedCount} have no served end time and are drawn open. No causal crossings are drawn because none are served. The thread table below is the accessible equivalent.`;
}

/** Composed empty state: the frame stays, so an empty weave reads as an
 * answered question rather than a broken page. */
function EmptyWeave({ undated, rows }: { undated: number; rows: number }) {
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center p-8">
      <div className="flex max-w-sm flex-col items-center gap-3 text-center">
        <span
          aria-hidden
          className="flex size-10 items-center justify-center border border-edge-subtle bg-surface-1 text-text-muted"
        >
          <Waypoints size={18} />
        </span>
        <h2 className="text-sm font-semibold tracking-tight">No thread to weave</h2>
        <p className="text-xs leading-relaxed text-text-muted">
          {rows === 0
            ? 'The session store answered and holds no sessions in this scope.'
            : `The store returned ${rows} ${rows === 1 ? 'session' : 'sessions'}, but ${undated} carried no usable start time — there is no honest position on the time axis for a session that never recorded when it began.`}{' '}
          <span className="text-text-secondary">
            Threads appear as soon as the store records a start time.
          </span>
        </p>
      </div>
    </div>
  );
}
