import { useQuery } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { Waypoints } from 'lucide-react';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import type { DashboardEnvelopeV1 } from '../../contracts/generated.ts';
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
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
import { formatDurationSeconds, formatMoment } from './tracks.ts';
import {
  composeWeave,
  extentOf,
  threadsFrom,
  type PlacedThread,
} from './weave.ts';
import {
  LcmTimelinePayloadV1Schema,
  type LoomSourceStatusV1,
  type LoomTemporalPayloadV1,
  LoomTemporalPayloadV1Schema,
} from '../../contracts/generated.ts';

/**
 * Loom — time and causality.
 *
 * The plan asks this surface for "interactive temporal and causal traces
 * linking prompts, reasoning, tools, subagents, code changes, branches,
 * commits, PRs, and outcomes". The daemon serves the first half of that
 * sentence. The temporal read now serves its persisted causal half with
 * provider-qualified rows. The reasoning, in full, is in `weave.ts`:
 *
 *   - Threads are real. Every mark is one session at its real start time, as
 *     thick as its real message count, in its host's column.
 *   - Extent is honest per thread. A recorded end wins, then a last-message
 *     observation; otherwise the thread stays visibly open.
 *   - Commit, edited-file and branch/worktree relations come directly from
 *     their durable authorities and retain provider and coverage.
 *
 * Selecting a thread isolates it and pulls its chain — prompt, turns, tools —
 * from the LCM session endpoint into the rail, then appends the selected
 * session's persisted edits, commit attributions and branch/worktree spans.
 */
export function LoomPage() {
  const scope = useScope((state) => state.scope);
  const temporal = useQuery({
    queryKey: ['loom', 'temporal', scopeKey(scope)],
    queryFn: () =>
      fetchEnvelope<LoomTemporalPayloadV1>(
        scopedUrl(scope, '/api/loom/temporal?limit=200'),
        LoomTemporalPayloadV1Schema,
      ),
  });
  const timeline = useLegacy(
    ['loom', 'timeline'],
    '/api/plugins/hermes-lcm/timeline',
    LcmTimelinePayloadV1Schema,
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
        note="sessions and durable causal relations on a measured time axis"
      />
      <TemporalBoundary pending={temporal.isPending} result={temporal.data}>
        {(envelope) => {
          const data = envelope.payload;
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
          const selectedCommits = selected
            ? data.commits.filter(
                (commit) =>
                  commit.provider === selected.host &&
                  commit.session_id === selected.sessionId,
              )
            : [];
          const selectedFiles = selected
            ? data.edited_files.filter(
                (file) =>
                  file.provider === selected.host && file.session_id === selected.sessionId,
              )
            : [];
          const selectedSpans = selected
            ? data.branch_spans.filter(
                (span) =>
                  span.provider === selected.host && span.session_id === selected.sessionId,
              )
            : [];
          const commitStatus =
            data.source_statuses.find((source) => source.id === 'session_commit') ?? null;
          const branchStatus =
            data.source_statuses.find((source) => source.id === 'branch_worktree') ?? null;
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
                    note: 'recorded end or last-message observation',
                    fraction:
                      weave.threads.length > 0
                        ? measuredEnds / weave.threads.length
                        : null,
                  },
                  {
                    label: 'window',
                    value: weave.extent
                      ? formatDurationSeconds(weave.extent.end - weave.extent.start)
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
                        Counts below are the persisted causal rows returned for
                        this exact session page. Provider, granularity and
                        coverage come from the temporal response.
                      </p>
                      {data.source_statuses.map((source) => (
                        <div key={source.id} className="flex flex-col gap-1">
                          <span className="td-legend text-text-secondary">
                            {source.label}
                          </span>
                          <StateChip
                            kind={source.state}
                            detail={sourceDetail(source)}
                          />
                          <span className="td-value truncate text-3xs text-text-muted">
                            {source.granularity}
                            {source.item_count == null ? '' : ` · ${source.item_count} rows`}
                          </span>
                        </div>
                      ))}
                    </div>
                  </Panel>

                  <Panel legend="Read identity">
                    <div className="flex flex-col gap-2">
                      <StateChip
                        kind={freshnessKind(envelope.freshness.state)}
                        detail={
                          envelope.freshness.observed_at_micros == null
                            ? 'observation time unrecorded'
                            : `observed ${formatMoment(envelope.freshness.observed_at_micros / 1_000_000)}`
                        }
                      />
                      <StateChip
                        kind={data.temporal_refresh.state}
                        detail={`${data.temporal_refresh.active_generations} active temporal generations · ${
                          data.temporal_refresh.latest_activated_at_micros == null
                            ? 'activation time unrecorded'
                            : `latest activation ${formatMoment(data.temporal_refresh.latest_activated_at_micros / 1_000_000)}`
                        } · ${data.temporal_refresh.authority}`}
                      />
                      <p className="text-3xs leading-relaxed text-text-muted">
                        {coverageDetail(envelope)}
                      </p>
                      <p className="text-3xs leading-relaxed text-text-muted">
                        {envelope.source_watermark
                          ? `${envelope.source_watermark.source} · ${envelope.source_watermark.watermark}`
                          : 'No temporal source watermark was recorded.'}
                      </p>
                    </div>
                  </Panel>

                  <ThreadChain
                    thread={selected}
                    relations={{
                      commits: selectedCommits,
                      editedFiles: selectedFiles,
                      branchSpans: selectedSpans,
                      commitStatus,
                      branchStatus,
                    }}
                  />
                </aside>
              </div>
            </div>
          );
        }}
      </TemporalBoundary>
    </div>
  );
}

function TemporalBoundary({
  pending,
  result,
  children,
}: {
  pending: boolean;
  result: EnvelopeResult<LoomTemporalPayloadV1> | undefined;
  children: (envelope: DashboardEnvelopeV1<LoomTemporalPayloadV1>) => ReactNode;
}) {
  // The three ways this read produces no envelope differ only in the chip they
  // carry; the plate they are centred on is one plate, written once.
  const plate = (kind: DomainStateKind, detail: string) => (
    <div className="flex min-h-0 flex-1 items-center justify-center p-8">
      <StateChip kind={kind} detail={detail} />
    </div>
  );
  if (pending) return plate('unknown', 'reading Loom temporal authorities');
  if (!result) return plate('offline', 'Loom temporal response unavailable');
  if (result.outcome === 'transport') {
    return plate(result.state, result.detail ?? 'Loom temporal response unavailable');
  }
  return children(result.envelope);
}

function sourceDetail(source: LoomSourceStatusV1): string {
  if (source.required_authority) return source.required_authority;
  const parts = [
    source.authority,
    source.providers.length > 0 ? `providers: ${source.providers.join(', ')}` : null,
    source.reason,
    source.coverage.eligible != null &&
    source.coverage.matched != null &&
    source.coverage.omitted != null
      ? `${source.coverage.matched}/${source.coverage.eligible} ${source.coverage.unit ?? 'items'} matched · ${source.coverage.omitted} omitted`
      : null,
    source.coverage.reason,
  ];
  return parts.filter((part): part is string => part != null && part.length > 0).join(' · ');
}

function coverageDetail(envelope: DashboardEnvelopeV1<LoomTemporalPayloadV1>): string {
  const { coverage } = envelope;
  const denominator =
    coverage.denominator == null
      ? 'denominator unrecorded'
      : `${formatCount(coverage.denominator)} ${coverage.unit ?? 'items'} eligible`;
  return `${coverage.completeness} coverage · ${formatCount(coverage.examined)} examined · ${formatCount(coverage.matched)} matched · ${denominator}`;
}

function freshnessKind(
  state: DashboardEnvelopeV1<LoomTemporalPayloadV1>['freshness']['state'],
): 'ready' | 'stale' | 'unknown' | 'unsupported' {
  switch (state) {
    case 'fresh':
      return 'ready';
    case 'stale':
      return 'stale';
    case 'unknown':
    case 'absent':
      return 'unknown';
    case 'unsupported':
      return 'unsupported';
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
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
      <div
        aria-label="Extent evidence legend"
        className="flex flex-wrap gap-x-4 gap-y-1 text-3xs text-text-muted"
      >
        <span className="flex items-center gap-1.5">
          <span aria-hidden className="h-0.5 w-3 bg-[var(--ev-measured)]" />
          measured session end
        </span>
        <span className="flex items-center gap-1.5">
          <span aria-hidden className="h-0.5 w-3 bg-[var(--ev-associated)]" />
          last-message observation
        </span>
        <span className="flex items-center gap-1.5">
          <span
            aria-hidden
            className="h-0.5 w-3 border-t border-dashed border-[var(--ev-unknown)]"
          />
          extent unknown
        </span>
      </div>
      <p className="text-2xs leading-relaxed text-text-muted">
        Each thread is one session: vertical position = when it started (exact,
        on the printed axis), column = the host that ran it, width = its message
        count on a log scale. A thread drawn solid to a lower edge has a served
        end time; a thread ending in a short dashed stub does not —{' '}
        <span className="text-text-secondary">
          {weave.openEndedCount} of {weave.threads.length} sessions have no
          recorded end or later message observation, so the stub marks
          unmeasured extent and its length means nothing.
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
      <div
        role="region"
        aria-label="Threads table"
        className="mt-1.5 max-h-72 overflow-auto border border-edge-subtle"
      >
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
                    // The only keyboard and touch path to selecting a thread —
                    // the weave canvas beside it is the picture, this is the
                    // control — so the row carries the touch minimum on its own
                    // box, in BOTH axes. At 320 the session column is the one
                    // the five-column table squeezes (38px), and the width half
                    // of the minimum is what stops it: the column holds 44 and
                    // the table scrolls inside its labelled region instead,
                    // which is the trade Plan 11 licenses.
                    className="flex min-h-[var(--touch-target-min)] w-full min-w-[var(--touch-target-min)] items-center gap-1.5 text-left"
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
                    ? formatDurationSeconds(thread.end - thread.start)
                    : 'unrecorded'}
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
  return `Weave: ${weave.threads.length} sessions as vertical threads, time running downward, one column per host (${hosts || 'none'}). ${weave.openEndedCount} have no recorded extent and are drawn open. Provider-qualified causal rows are served and listed in the selected-thread rail, but are not geometrically drawn on this weave. The thread table below is the accessible equivalent.`;
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
