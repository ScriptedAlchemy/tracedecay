import { z } from 'zod';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { ReadFailure } from '../../ui/LegacyStates.tsx';
import { LegacyBoundary } from '../../ui/ReadSection.tsx';
import { MeterRow, ReadoutBar } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { AnalyticsUsageSummaryV1Schema } from '../../contracts/generated.ts';
import { logFraction } from '../../viz/scale.ts';
import {
  ANALYTICS_EVENT_LIMIT,
  describeWindow,
  familiesSummary,
  familyVerdict,
  FAMILY_NOTES,
  formatSpan,
  percent,
  summarizeDominance,
  type FamilyRow,
  type UsageRow,
} from './usage.ts';

const BASE = '/api/plugins/analytics';

const HintsPayload = z
  .object({
    available: z.boolean().optional(),
    families: z
      .array(
        z
          .object({
            family: z.string(),
            usage_events: z.number().optional(),
            relevant_events: z.number().optional(),
            missed_events: z.number().optional(),
            underused: z.boolean().optional(),
          })
          .passthrough(),
      )
      .optional(),
  })
  .passthrough();

/** `{ <label field>: string, count: number }` — the shape every `by_*` array on
 * the diagnostics payload uses, differing only in the name of the label field.
 * Kept loose (`passthrough`, unknown values) so a new label field on the wire
 * never fails the whole page's parse. */
const CountRows = z.array(z.record(z.string(), z.unknown()));

/**
 * `/api/plugins/analytics/diagnostics`. The only endpoint on this plugin that
 * carries a clock: `events_per_hour` over the counted window, and the twenty
 * most recent events with their own timestamps. It is also the slowest — it
 * folds the full hook-analytics JSONL (over a million rows here) — so it lives
 * behind its own boundary and the fast plates render without waiting for it.
 */
const DiagnosticsPayload = z
  .object({
    available: z.boolean().optional(),
    event_count: z.number().optional(),
    events_per_hour: z.number().optional(),
    hook_call_count: z.number().optional(),
    mcp_tool_call_count: z.number().optional(),
    by_event_kind: CountRows.optional(),
    by_outcome: CountRows.optional(),
    by_mcp_tool: CountRows.optional(),
    recent_events: z
      .array(
        z
          .object({
            timestamp: z.number(),
            event_kind: z.string().optional(),
            tool_name: z.string().optional(),
            outcome: z.string().optional(),
          })
          .passthrough(),
      )
      .optional(),
    hook_window: z
      .object({
        window_rows: z.number(),
        rows_scanned: z.number(),
        rows_included: z.number(),
        truncated: z.boolean(),
        total_rows_known: z.boolean().optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();

/**
 * Agents: what connected agents actually did, over the window the analytics
 * store will admit to.
 *
 * Two things dictate the whole composition. First, the event distribution is
 * degenerate — one category carries around nine in ten events — so the leading
 * plate STATES the dominance and draws the remainder on a log band, rather
 * than plotting a linear axis on which eleven of twelve rows are a sliver.
 * Second, the `10,000` on every count is `ANALYTICS_EVENT_LIMIT`, not a total;
 * it is captioned as the cap it is everywhere it appears, and the window it
 * covers is stated in hours beside it.
 */
export function AgentsPage() {
  // `analytics_api::usage` answers with the same body the overview route
  // decodes as `AnalyticsUsageSummaryV1`, so this read is contracted even
  // though the handler's own return type is still `Value`. Its three siblings
  // below — underused, hints and diagnostics — are not modelled in Rust at all.
  const usage = useLegacy(
    ['analytics', 'usage'],
    `${BASE}/usage`,
    AnalyticsUsageSummaryV1Schema,
  );
  const hints = useLegacy(['analytics', 'underused'], `${BASE}/underused`, HintsPayload);
  const diagnostics = useLegacy(
    ['analytics', 'diagnostics'],
    `${BASE}/diagnostics`,
    DiagnosticsPayload,
    // The hook-analytics fold is ~14s against a real store. Once is enough per
    // visit; a refetch interval here would keep a reader's browser and the
    // daemon both busy for no new reading.
    { staleTime: 5 * 60_000 },
  );

  return (
    <LegacyBoundary title="Agents" pending={usage.isPending} result={usage.data}>
      {(data) => {
        const rows: UsageRow[] = data.by_category;
        const dominance = summarizeDominance(rows);
        const diag =
          diagnostics.data?.outcome === 'ok' ? diagnostics.data.data : undefined;
        const diagnosticsAvailable = diag?.available !== false;
        const diagnosticsRead = diagnosticsAvailable ? diag : undefined;
        const window = describeWindow(
          data.available ? data.event_count : diagnosticsRead?.event_count,
          diagnosticsRead?.events_per_hour,
        );
        const hookNote = diagnosticsRead?.hook_window?.truncated
          ? `recent suffix · ${diagnosticsRead.hook_window.rows_scanned.toLocaleString()} rows scanned`
          : diagnosticsRead?.hook_window
            ? `${diagnosticsRead.hook_window.rows_scanned.toLocaleString()} hook rows scanned`
            : diagnosticsRead
              ? 'hook window unreported'
              : 'analytics diagnostics unavailable';
        return (
          <div
            className="flex h-full flex-col overflow-auto"
            tabIndex={0}
            role="region"
            aria-label="Agents content"
          >
            <div className="flex items-baseline gap-3 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Agents</h1>
              <span className="min-w-0 truncate text-2xs text-text-muted">
                {data.available
                  ? window.events == null
                    ? 'session-message fallback · event count unavailable'
                    : `analytics_events · ${window.capped ? `most recent ${ANALYTICS_EVENT_LIMIT.toLocaleString()} (endpoint cap)` : `${window.events.toLocaleString()} events`}`
                  : 'analytics store unavailable'}
              </span>
            </div>

            {/* The window itself, which the page previously never stated. Every
              * count below is taken inside it, and the endpoint refuses to
              * count past `ANALYTICS_EVENT_LIMIT` — so the cap, the span and
              * the rate are the frame the rest of the page hangs in, not a
              * footnote. */}
            <ReadoutBar
              label="Event window"
              size="xl"
              elevation="raised"
              items={[
                {
                  label: window.capped ? 'events (capped)' : 'events',
                  value: window.events?.toLocaleString() ?? '—',
                  note: window.capped
                    ? 'the endpoint counts no further back'
                    : window.events == null
                      ? 'event count unavailable'
                      : 'every event on record',
                },
                {
                  label: 'window',
                  value: formatSpan(window.spanHours),
                  note: window.spanHours != null ? 'derived: events ÷ rate' : 'rate not served',
                },
                {
                  label: 'rate',
                  value: window.perHour != null ? Math.round(window.perHour).toLocaleString() : '—',
                  unit: window.perHour != null ? '/h' : undefined,
                  note: 'events per hour, served',
                },
                {
                  label: 'mcp tool calls',
                  value: exact(diagnosticsRead?.mcp_tool_call_count),
                  note: 'inside the window',
                },
                {
                  label: 'hook calls',
                  value: exact(diagnosticsRead?.hook_call_count),
                  note: hookNote,
                },
              ]}
            />

            <OverviewGrid>
              <OverviewCard title="Where the events go">
                {!data.available ? (
                  <ReadFailure label="Usage analytics unavailable" />
                ) : window.events == null && rows.length === 0 ? (
                  <ReadFailure label="Categorized usage events unavailable" />
                ) : (
                  // The window's own count, or null. Substituting the
                  // categorized total made the two agree by construction, which
                  // is a claim that every event was categorized.
                  <CategoryComposition dominance={dominance} counted={window.events} />
                )}
              </OverviewCard>

              <OverviewCard title="Most-called tools">
                <LegacyBoundary
                  title="Tools"
                  pending={diagnostics.isPending}
                  result={diagnostics.data}
                >
                  {(payload) =>
                    payload.available === false ? (
                      <ReadFailure label="Analytics diagnostics unavailable" />
                    ) : (
                      <ToolRanking rows={payload.by_mcp_tool ?? []} />
                    )
                  }
                </LegacyBoundary>
              </OverviewCard>

              <OverviewCard title="Latest events">
                <LegacyBoundary
                  title="Events"
                  pending={diagnostics.isPending}
                  result={diagnostics.data}
                >
                  {(payload) =>
                    payload.available === false ? (
                      <ReadFailure label="Analytics diagnostics unavailable" />
                    ) : (
                      <RecentTape rows={payload.recent_events ?? []} />
                    )
                  }
                </LegacyBoundary>
              </OverviewCard>

              <OverviewCard title="What the window is made of">
                <LegacyBoundary
                  title="Composition"
                  pending={diagnostics.isPending}
                  result={diagnostics.data}
                >
                  {(payload) =>
                    payload.available === false ? (
                      <ReadFailure label="Analytics diagnostics unavailable" />
                    ) : (
                      <WindowComposition
                        kinds={payload.by_event_kind ?? []}
                        outcomes={payload.by_outcome ?? []}
                        counted={payload.event_count ?? window.events}
                      />
                    )
                  }
                </LegacyBoundary>
              </OverviewCard>

              <OverviewCard title="Tool families the hint engine watches">
                <LegacyBoundary title="Hints" pending={hints.isPending} result={hints.data}>
                  {(hintData) =>
                    hintData.available === false ? (
                      <ReadFailure label="Hint diagnostics unavailable" />
                    ) : (
                      <FamilyList rows={hintData.families ?? []} />
                    )
                  }
                </LegacyBoundary>
              </OverviewCard>
            </OverviewGrid>
          </div>
        );
      }}
    </LegacyBoundary>
  );
}

/**
 * The composition plate. Its whole job is to not lie about a 6,774-to-1
 * distribution.
 *
 * The leader is not drawn at all: a bar at 100% of the band beside eleven
 * slivers is a picture of nothing. It is stated, in words, with its share. The
 * remainder then gets the band to itself on a LOG scale — which is captioned
 * as logarithmic, because a length the reader cannot compare linearly must say
 * so or it is worse than no length at all.
 */
function CategoryComposition({
  dominance,
  counted,
}: {
  dominance: ReturnType<typeof summarizeDominance>;
  /** The window's total event count, or null when the endpoint served none —
   * in which case how much of the window these categories cover is unknown. */
  counted: number | null;
}) {
  const { leader, leaderShare, rest, total, spread } = dominance;
  if (!leader || total === 0) {
    return <p className="text-2xs text-text-muted">no analytics events recorded</p>;
  }
  const share = percent(leaderShare);
  const smallest = rest[rest.length - 1];
  const uncategorized = counted != null ? Math.max(0, counted - total) : null;
  const restCeiling = rest.reduce((max, row) => Math.max(max, row.events), 0);
  return (
    <div className="flex flex-col gap-3">
      <p className="text-xs leading-relaxed text-text-primary">
        <span className="td-value">{leader.category}</span> is{' '}
        <span className="td-value">{share != null ? `${share}%` : '—'}</span> of all
        categorized events — {leader.events.toLocaleString()} of{' '}
        {total.toLocaleString()}
        {spread != null && spread >= 10 && smallest
          ? `, and ${Math.round(spread).toLocaleString()}× the smallest (${smallest.category}, ${smallest.events.toLocaleString()}).`
          : '.'}
      </p>
      {counted == null ? (
        <p className="text-2xs leading-relaxed text-text-muted">
          The window's own event count was not reported, so how many of its events
          carry no tool or skill to categorize is unknown — these{' '}
          {total.toLocaleString()} categorized events are not known to be all of
          them.
        </p>
      ) : uncategorized != null && uncategorized > 0 ? (
        <p className="text-2xs leading-relaxed text-text-muted">
          {uncategorized.toLocaleString()} of the {counted.toLocaleString()} events in
          the window carry no tool or skill to categorize (hook routing), so they are
          absent from this plate rather than folded into an “other”.
        </p>
      ) : null}
      {rest.length > 0 ? (
        <figure className="flex flex-col gap-1.5">
          <figcaption className="td-legend">
            everything else · log scale
          </figcaption>
          {rest.map((row) => (
            <MeterRow
              key={`${row.kind}:${row.category}`}
              leading={
                <span className="td-legend w-12 shrink-0 truncate max-sm:hidden">{row.kind}</span>
              }
              label={row.category}
              fraction={logFraction(row.events, restCeiling)}
              value={row.events.toLocaleString()}
            />
          ))}
          <figcaption className="text-3xs leading-relaxed text-text-muted">
            The leader is stated above rather than drawn: on one shared linear axis
            every row here would be under two pixels. These rails are log(1+x) against{' '}
            {restCeiling.toLocaleString()}, so order is readable but lengths are not
            proportional to the counts beside them.
          </figcaption>
        </figure>
      ) : null}
    </div>
  );
}

/** The 136 distinct MCP tools the window recorded, ranked. Same log band and
 * the same caption obligation as the composition plate — this distribution is
 * even longer-tailed (1,945 to 1). */
function ToolRanking({ rows }: { rows: ReadonlyArray<Record<string, unknown>> }) {
  const ranked = [...rows]
    .map((row) => ({
      name: String(row['tool_name'] ?? ''),
      count: Number(row['count'] ?? 0),
    }))
    .filter((row) => row.name !== '' && Number.isFinite(row.count))
    .sort((a, b) => b.count - a.count);
  if (ranked.length === 0) {
    return <p className="text-2xs text-text-muted">no tool calls recorded in this window</p>;
  }
  const shown = ranked.slice(0, 12);
  const ceiling = ranked[0]?.count ?? 0;
  const tail = ranked.length - shown.length;
  return (
    <figure className="flex flex-col gap-1.5">
      <figcaption className="td-legend">
        top {shown.length} of {ranked.length} tools · log scale
      </figcaption>
      {shown.map((row) => (
        <MeterRow
          key={row.name}
          label={row.name}
          title={row.name}
          fraction={logFraction(row.count, ceiling)}
          value={row.count.toLocaleString()}
        />
      ))}
      {tail > 0 ? (
        <figcaption className="text-3xs leading-relaxed text-text-muted">
          {tail.toLocaleString()} further tools were called between{' '}
          {(ranked[ranked.length - 1]?.count ?? 0).toLocaleString()} and{' '}
          {(ranked[shown.length]?.count ?? 0).toLocaleString()} times each, and are not
          drawn.
        </figcaption>
      ) : null}
    </figure>
  );
}

/** The time dimension, honestly bounded: the endpoint serves twenty events
 * with real timestamps and nothing between them, so this is a tape of the
 * latest few — not a series, and it does not pretend to be one.
 *
 * The rows carry a clock time only. Twenty repetitions of the same calendar
 * date down a strip that spans four minutes is twenty copies of one fact; the
 * date is stated once, in the caption, where it belongs. */
function RecentTape({ rows }: { rows: ReadonlyArray<Record<string, unknown>> }) {
  const events = rows
    .map((row) => ({
      timestamp: Number(row['timestamp'] ?? 0),
      kind: String(row['event_kind'] ?? ''),
      tool: String(row['tool_name'] ?? ''),
      outcome: String(row['outcome'] ?? ''),
    }))
    .filter((row) => Number.isFinite(row.timestamp) && row.timestamp > 0);
  if (events.length === 0) {
    return <p className="text-2xs text-text-muted">no recent events served</p>;
  }
  // Twelve, not twenty: this plate shares a grid row with two others, and a
  // strip half again as tall as its neighbours buys nothing but the void
  // underneath them. The count served is stated so the truncation is visible.
  const shown = events.slice(0, 12);
  const newest = events[0]!.timestamp;
  const oldest = events[events.length - 1]!.timestamp;
  return (
    <figure className="flex flex-col gap-1.5">
      <figcaption className="td-legend">
        latest {shown.length} of {events.length} · {formatDay(newest)} ·{' '}
        {formatSpan((newest - oldest) / 3600)}
      </figcaption>
      <ol className="flex flex-col">
        {shown.map((event, index) => (
          <li
            key={`${event.timestamp}-${index}`}
            className="flex items-baseline gap-2 border-b border-edge-subtle py-1 text-2xs last:border-b-0"
          >
            <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
              {formatClock(event.timestamp)}
            </span>
            <span className="min-w-0 flex-1 truncate text-text-primary" title={event.tool}>
              {event.tool || event.kind || '—'}
            </span>
            <span
              // The state word is the signal, not its hue: `--raw-state-stale`
              // at the legend tier (10px) measures 4.39:1 on the light
              // substrate, just under AA, and every element on this row is at
              // that tier. Weight and the word itself carry it instead.
              className={cn(
                'td-legend shrink-0',
                event.outcome === 'error' ? 'text-text-primary' : 'text-text-muted',
              )}
            >
              {event.outcome || event.kind}
            </span>
          </li>
        ))}
      </ol>
      <figcaption className="text-3xs leading-relaxed text-text-muted">
        The endpoint serves these events and the window's average rate; it serves no
        per-interval counts, so nothing here is drawn as a series.
      </figcaption>
    </figure>
  );
}

/** `HH:MM:SS` in local time. The tape's rows span minutes, so seconds are the
 * unit that separates them and the date is constant across the strip. */
function formatClock(epochSeconds: number): string {
  const date = new Date(epochSeconds * 1000);
  if (Number.isNaN(date.getTime())) return '—';
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** The calendar date the tape's clock times belong to, stated once. */
function formatDay(epochSeconds: number): string {
  const date = new Date(epochSeconds * 1000);
  if (Number.isNaN(date.getTime())) return '—';
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(
    date.getDate(),
  ).padStart(2, '0')}`;
}

/** A served count printed in full, or an em dash when the read has not landed.
 * These are provenance figures — the point of them is the exact number. */
function exact(value: number | null | undefined): string {
  return value != null && Number.isFinite(value) ? value.toLocaleString() : '—';
}

/** Accounts for the whole capped window: which kinds it is made of and how
 * they came out. This is what makes the categorized total above legible as a
 * subset rather than a discrepancy. */
function WindowComposition({
  kinds,
  outcomes,
  counted,
}: {
  kinds: ReadonlyArray<Record<string, unknown>>;
  outcomes: ReadonlyArray<Record<string, unknown>>;
  /** The denominator these shares are of, or null when neither the diagnostics
   * fold nor the usage read served a window count. */
  counted: number | null;
}) {
  const kindRows = kinds
    .map((row) => ({ label: String(row['event_kind'] ?? ''), count: Number(row['count'] ?? 0) }))
    .filter((row) => row.label !== '')
    .sort((a, b) => b.count - a.count);
  const outcomeRows = outcomes
    .map((row) => ({ label: String(row['outcome'] ?? ''), count: Number(row['count'] ?? 0) }))
    .filter((row) => row.label !== '')
    .sort((a, b) => b.count - a.count);
  if (kindRows.length === 0 && outcomeRows.length === 0) {
    return <p className="text-2xs text-text-muted">the window reported no composition</p>;
  }
  return (
    <div className="flex flex-col gap-3">
      <ShareRows legend="by event kind" rows={kindRows} total={counted} />
      <ShareRows legend="by outcome" rows={outcomeRows} total={counted} />
    </div>
  );
}

/** A small set of parts of one known whole. Linear is correct here — these
 * shares are of the same denominator and none of them is a sliver.
 *
 * When the denominator was not served the counts are still real, so they are
 * printed; it is the "share of N" clause that goes, because there is no N. */
function ShareRows({
  legend,
  rows,
  total,
}: {
  legend: string;
  rows: ReadonlyArray<{ label: string; count: number }>;
  total: number | null;
}) {
  if (rows.length === 0) return null;
  return (
    <figure className="flex flex-col gap-1.5">
      <figcaption className="td-legend">
        {legend}
        {total != null ? ` · share of ${total.toLocaleString()}` : ' · window total unreported'}
      </figcaption>
      {rows.map((row) => (
        <MeterRow
          key={row.label}
          label={row.label}
          fraction={total != null && total > 0 ? row.count / total : null}
          value={row.count.toLocaleString()}
        />
      ))}
    </figure>
  );
}

/**
 * The families plate, made actionable.
 *
 * It used to print four snake_case identifiers with no counts, no meaning and
 * no verdict — a list of words. Each row now says what the family covers, what
 * would count as reaching for a substitute instead, and where this window
 * actually landed. Two of the four have no substitute detector at all, which
 * means they are pinned at "not under-used" by construction; that is stated
 * rather than allowed to read as a clean bill of health.
 */
function FamilyList({ rows }: { rows: readonly FamilyRow[] }) {
  if (rows.length === 0) {
    return <p className="text-2xs text-text-muted">no tool families reported</p>;
  }
  const summary = familiesSummary(rows);
  const ordered = [...rows].sort((a, b) => (b.missed_events ?? 0) - (a.missed_events ?? 0));
  return (
    <div className="flex flex-col gap-3">
      {summary ? (
        <p className="text-xs leading-relaxed text-text-primary">{summary}</p>
      ) : null}
      <ul className="flex flex-col">
        {ordered.map((row) => {
          const verdict = familyVerdict(row);
          const note = FAMILY_NOTES[row.family];
          return (
            <li
              key={row.family}
              className="flex flex-col gap-0.5 border-b border-edge-subtle py-1.5 last:border-b-0"
            >
              <span className="flex items-baseline gap-2">
                <span className="td-value min-w-0 flex-1 truncate text-xs text-text-primary">
                  {row.family.replace(/_/g, ' ')}
                </span>
                <span
                  // Same reason as the tape's outcome column: a hue that fails
                  // AA at 10px is not a signal, it is a defect. The three
                  // verdicts are separated by text weight and by the word.
                  className={cn(
                    'td-legend shrink-0',
                    verdict.state === 'underused'
                      ? 'text-text-primary'
                      : verdict.state === 'covered'
                        ? 'text-text-secondary'
                        : 'text-text-muted',
                  )}
                >
                  {verdict.state}
                </span>
              </span>
              {note ? (
                <span className="td-value truncate text-3xs text-text-muted" title={note.covers}>
                  {note.covers}
                </span>
              ) : null}
              <span className="text-2xs leading-relaxed text-text-secondary">
                {verdict.line}
              </span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
