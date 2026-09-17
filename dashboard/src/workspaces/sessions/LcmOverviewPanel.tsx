/**
 * LCM OVERVIEW — the right-hand register when no session is selected.
 *
 * Four independent reads, each stated with its own coverage and never summed
 * into one another: the LCM timeline (temporal scope and token provenance),
 * the LCM overview (the canonical hydrated window the daemon drained for this
 * read — sessions, messages, providers, roles, compaction), and the index page
 * (which slice of the retained session store is on screen). The overview's
 * counts describe the drained window, not the whole store; the index's
 * `total` is the store's own count. Both are printed with their denominators.
 */
import {
  assertNever,
  type DashboardEnvelopeV1,
  type LcmOverviewPayloadV1,
  type LcmTimelinePayloadV1,
  type LoomTemporalPayloadV1,
} from '../../contracts/generated.ts';
import { InspectorPanel } from '../../ui/archetypes/ExplorerSplit.tsx';
import { toStripCoverage, toStripFreshness } from '../../ui/EnvelopeTruth.tsx';
import { EvidenceTruthStrip } from '../../ui/EvidenceTruthStrip.tsx';
import type { ReadState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip';
import { Fact, Legend, MeterRow, Readout } from '../../ui/instrument.tsx';
import { splitCount } from '../../ui/format.ts';
import { pageBounds, provenanceTally, type RowsPerPage, type TimelineBucket } from './model.ts';

export interface LcmOverviewPanelProps {
  overview: ReadState<DashboardEnvelopeV1<LcmOverviewPayloadV1>>;
  timeline: ReadState<DashboardEnvelopeV1<LcmTimelinePayloadV1>>;
  index: ReadState<DashboardEnvelopeV1<LoomTemporalPayloadV1>>;
  bucket: TimelineBucket;
  page: number;
  rows: RowsPerPage;
}

export function LcmOverviewPanel({ overview, timeline, index, bucket, page, rows }: LcmOverviewPanelProps) {
  return (
    <InspectorPanel
      title="LCM overview"
      eyebrow={
        overview.kind === 'ready' ? (
          <StateChip kind={overview.value.domain_state} />
        ) : (
          <StateChip kind={overview.state} />
        )
      }
    >
      <div className="flex flex-col gap-4">
        <TemporalScope timeline={timeline} bucket={bucket} />
        <LoadedWindow overview={overview} />
        <TokenProvenance timeline={timeline} />
        <IndexPageStatus index={index} page={page} rows={rows} />
      </div>
    </InspectorPanel>
  );
}

function bucketNoun(bucket: TimelineBucket, count: number): string {
  switch (bucket) {
    case 'day':
      return count === 1 ? 'day' : 'days';
    case 'hour':
      return count === 1 ? 'hour' : 'hours';
    default:
      return assertNever(bucket);
  }
}

function TemporalScope({
  timeline,
  bucket,
}: {
  timeline: ReadState<DashboardEnvelopeV1<LcmTimelinePayloadV1>>;
  bucket: TimelineBucket;
}) {
  return (
    <section aria-label="Temporal scope" className="flex flex-col gap-2">
      <Legend>temporal scope</Legend>
      {timeline.kind === 'blocked' ? (
        <StateChip kind={timeline.state} detail={timeline.detail} />
      ) : timeline.value.payload.exists === false ? (
        <StateChip kind="unknown" detail="LCM session store is unavailable; scope is unknown" />
      ) : (
        <TemporalScopeFacts payload={timeline.value.payload} bucket={bucket} />
      )}
    </section>
  );
}

function TemporalScopeFacts({ payload, bucket }: { payload: LcmTimelinePayloadV1; bucket: TimelineBucket }) {
  const first = payload.buckets[0];
  const last = payload.buckets.at(-1);
  const coverage = payload.coverage;
  if (!first || !last) {
    return <StateChip kind="complete_zero_findings" detail="no dated buckets in the loaded window" />;
  }
  return (
    <dl className="grid grid-cols-2 gap-x-3 gap-y-2 text-2xs">
      <Fact label="first loaded bucket" value={first.bucket} />
      <Fact label="last loaded bucket" value={last.bucket} />
      <div className="col-span-2 flex min-w-0 flex-col gap-0.5">
        <dt className="td-legend">loaded window</dt>
        <dd className="text-3xs text-text-secondary tabular">
          {coverage
            ? `${coverage.returned_buckets.toLocaleString()} of ${coverage.total_dated_buckets.toLocaleString()} dated ${bucketNoun(bucket, coverage.total_dated_buckets)} (limit ${coverage.limit.toLocaleString()})${coverage.truncated ? ' · older buckets omitted' : ''}`
            : `${payload.buckets.length.toLocaleString()} dated ${bucketNoun(bucket, payload.buckets.length)} · coverage not reported`}
        </dd>
      </div>
    </dl>
  );
}

function LoadedWindow({ overview }: { overview: ReadState<DashboardEnvelopeV1<LcmOverviewPayloadV1>> }) {
  return (
    <section aria-label="Loaded LCM window" className="flex flex-col gap-2">
      <Legend>loaded lcm window</Legend>
      {overview.kind === 'blocked' ? (
        <StateChip kind={overview.state} detail={overview.detail} />
      ) : overview.value.payload.exists === false ? (
        <StateChip kind="unknown" detail="LCM session store is unavailable; session count is unknown" />
      ) : (
        <LoadedWindowFacts envelope={overview.value} />
      )}
    </section>
  );
}

function LoadedWindowFacts({ envelope }: { envelope: DashboardEnvelopeV1<LcmOverviewPayloadV1> }) {
  const stats = envelope.payload.overview;
  const sessions = splitCount(stats.sessions_total);
  const messages = splitCount(stats.messages_total);
  const compression = stats.compression;
  return (
    <div className="flex flex-col gap-3">
      <div className="grid grid-cols-2 gap-2">
        <div className="td-raised border border-edge-subtle px-2.5 py-2">
          <Readout label="sessions" size="sm" value={sessions.value} unit={sessions.unit} />
        </div>
        <div className="td-raised border border-edge-subtle px-2.5 py-2">
          <Readout label="messages" size="sm" value={messages.value} unit={messages.unit} />
        </div>
      </div>
      <EvidenceTruthStrip
        coverage={toStripCoverage(envelope.coverage)}
        freshness={toStripFreshness(envelope.freshness)}
        omissions={envelope.coverage.omitted ?? undefined}
      />
      {envelope.coverage.omission_reasons.map((reason) => (
        <p key={reason} className="text-3xs text-text-muted">
          {reason}
        </p>
      ))}
      <p className="text-3xs leading-snug text-text-muted">
        Counts describe the canonical hydrated records the daemon drained for this read
        {envelope.coverage.unit ? ` (${envelope.coverage.unit})` : ''}, not the whole store.
      </p>

      <div className="flex flex-col gap-1">
        <Legend>messages by provider</Legend>
        {stats.source_counts.length === 0 ? (
          <StateChip kind="complete_zero_findings" detail="no provider recorded in the window" />
        ) : (
          stats.source_counts.map((source) => (
            <MeterRow
              key={source.source}
              label={source.source}
              value={source.count.toLocaleString()}
              fraction={stats.messages_total > 0 ? source.count / stats.messages_total : null}
              figureWidth="wide"
            />
          ))
        )}
      </div>

      <div className="flex flex-col gap-1">
        <Legend>messages by role</Legend>
        {stats.role_counts.length === 0 ? (
          <StateChip kind="complete_zero_findings" detail="no role recorded in the window" />
        ) : (
          stats.role_counts.map((role) => (
            <MeterRow
              key={role.role ?? 'unrecorded'}
              label={role.role ?? 'role unrecorded'}
              value={role.count.toLocaleString()}
              fraction={stats.messages_total > 0 ? role.count / stats.messages_total : null}
              figureWidth="wide"
            />
          ))
        )}
      </div>

      <dl className="grid grid-cols-2 gap-x-3 gap-y-2 text-2xs">
        <Fact label="summary nodes" value={stats.summary_nodes_total.toLocaleString()} />
        <Fact label="sessions compacted" value={stats.summary_node_sessions_total.toLocaleString()} />
        <Fact label="max summary depth" value={String(stats.max_summary_depth)} />
        <Fact
          label="compaction"
          value={
            compression.source_token_count != null && compression.token_count != null
              ? `${compression.token_count.toLocaleString()} ← ${compression.source_token_count.toLocaleString()} tokens`
              : 'token counts unavailable'
          }
          muted={compression.source_token_count == null || compression.token_count == null}
        />
      </dl>
    </div>
  );
}

function TokenProvenance({ timeline }: { timeline: ReadState<DashboardEnvelopeV1<LcmTimelinePayloadV1>> }) {
  return (
    <section aria-label="Token provenance" className="flex flex-col gap-2">
      <Legend>token provenance · loaded window</Legend>
      {timeline.kind === 'blocked' ? (
        <StateChip kind={timeline.state} detail={timeline.detail} />
      ) : timeline.value.payload.exists === false ? (
        <StateChip kind="unknown" detail="LCM session store is unavailable" />
      ) : (
        <TokenProvenanceRows payload={timeline.value.payload} />
      )}
    </section>
  );
}

function TokenProvenanceRows({ payload }: { payload: LcmTimelinePayloadV1 }) {
  const tally = provenanceTally(payload.buckets, payload.undated);
  const total = tally.known + tally.unknown;
  if (total === 0) {
    return <StateChip kind="complete_zero_findings" detail="no messages in the loaded window" />;
  }
  const share = (count: number) => `${((count / total) * 100).toFixed(1)}%`;
  return (
    <div className="flex flex-col gap-1" data-token-provenance>
      <MeterRow
        label="o200k approximate"
        title="counted with the o200k tokenizer; approximate by construction"
        value={tally.known.toLocaleString()}
        fraction={tally.known / total}
        figureWidth="wide"
        leading={<span className="td-value w-12 shrink-0 text-3xs text-text-muted">{share(tally.known)}</span>}
      />
      <MeterRow
        label="unavailable"
        title="messages whose token count the store disclaims"
        value={tally.unknown.toLocaleString()}
        fraction={tally.unknown / total}
        tone="bg-state-partial"
        figureWidth="wide"
        leading={<span className="td-value w-12 shrink-0 text-3xs text-text-muted">{share(tally.unknown)}</span>}
      />
      <p className="text-3xs text-text-muted tabular">
        {total.toLocaleString()} messages · {tally.dated.toLocaleString()} dated ·{' '}
        {tally.undated.toLocaleString()} undated (held separately from the field)
      </p>
    </div>
  );
}

function IndexPageStatus({
  index,
  page,
  rows,
}: {
  index: ReadState<DashboardEnvelopeV1<LoomTemporalPayloadV1>>;
  page: number;
  rows: RowsPerPage;
}) {
  return (
    <section aria-label="Index page" className="flex flex-col gap-2">
      <Legend>index page</Legend>
      {index.kind === 'blocked' ? (
        <StateChip kind={index.state} detail={index.detail} />
      ) : index.value.payload.available === false ? (
        <StateChip kind="unknown" detail="session store not readable" />
      ) : (
        <IndexPageFacts envelope={index.value} page={page} rows={rows} />
      )}
    </section>
  );
}

function IndexPageFacts({
  envelope,
  page,
  rows,
}: {
  envelope: DashboardEnvelopeV1<LoomTemporalPayloadV1>;
  page: number;
  rows: RowsPerPage;
}) {
  const payload = envelope.payload;
  const bounds = pageBounds(page, rows, payload.sessions.length, payload.total);
  return (
    <div className="flex flex-col gap-2">
      <dl className="grid grid-cols-2 gap-x-3 gap-y-2 text-2xs">
        <Fact label="page" value={`${page} of ${bounds.pageCount ?? '?'}`} />
        <Fact label="rows per page" value={String(rows)} />
        <Fact
          label="loaded rows"
          value={bounds.first != null && bounds.last != null ? `${bounds.first}–${bounds.last}` : 'none'}
          muted={bounds.first == null}
        />
        <Fact label="sessions in store" value={payload.total.toLocaleString()} />
      </dl>
      <EvidenceTruthStrip
        coverage={toStripCoverage(envelope.coverage)}
        freshness={toStripFreshness(envelope.freshness)}
        omissions={envelope.coverage.omitted ?? undefined}
      />
      <StateChip
        kind={payload.temporal_refresh.state}
        detail={`temporal refresh · ${payload.temporal_refresh.active_generations.toLocaleString()} active generations`}
      />
    </div>
  );
}
