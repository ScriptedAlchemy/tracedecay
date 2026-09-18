import {
  assertNever,
  type DashboardCoverageV1,
  type DashboardEnvelopeV1,
  type StorageTelemetryPayloadV1,
  type StorageTelemetryReadV1,
  type StoreTelemetryEntryV1,
  type TableGrowthDimensionV1,
  type TableGrowthThresholdV1,
} from '../../contracts/generated.ts';
import { CapacityBar } from '../../ui/ActivityColumns.tsx';
import { EnvelopeTruth } from '../../ui/EnvelopeTruth.tsx';
import { ReadModelState } from '../../ui/ReadSection.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import {
  budgetPresentation,
  dimensionDotClass,
  formatBytes,
  growthPresentation,
  storeRolesLabel,
  tableGrowthOmissionPresentation,
  tableGrowthPresentation,
  type DimensionPresentation,
} from './storageModel.ts';

/** The exact store-telemetry read model: one card per distinct store file,
 * each carrying its typed read, measured size, budget, growth, and per-table
 * growth. Nothing here recomputes health; the server's states are printed. */
export function TelemetryReadModel({
  envelope,
  refreshing,
  onRefresh,
}: {
  envelope: DashboardEnvelopeV1<StorageTelemetryPayloadV1>;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  return (
    <>
      <EnvelopeTruth envelope={envelope} refreshing={refreshing} onRefresh={onRefresh} />
      {envelope.payload.stores.length === 0 ? (
        <ReadModelState kind="unknown" detail="telemetry payload contained no stores" />
      ) : (
        <>
          <TableGrowthFleetCoverage coverage={envelope.payload.table_growth_coverage} />
          <OverviewGrid>
            {/* One card per distinct store *file*: roles that share a database
                are merged server-side, so the path is the stable identity. */}
            {envelope.payload.stores.map((store) => (
              <StoreCard
                key={store.path}
                entry={store}
                tableGrowthThreshold={envelope.payload.table_growth_threshold}
              />
            ))}
          </OverviewGrid>
        </>
      )}
      <ReadModelNotes notes={[
        `budgets: ${envelope.payload.budget_note}`,
        `growth: ${envelope.payload.growth_note}`,
      ]} />
    </>
  );
}

/** Aggregate table-growth coverage for the whole read. The store cards below
 * carry per-store detail; this is the only place that says how much of the
 * fleet the per-table view actually covers, so a run where four of five stores
 * never produced a comparison cannot read as a complete picture. */
function TableGrowthFleetCoverage({ coverage }: { coverage: DashboardCoverageV1 }) {
  const complete = coverage.completeness === 'complete';
  return (
    <section
      className="mx-4 mt-3 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 p-3"
      aria-label="Table growth coverage across all stores"
      data-table-growth-coverage={coverage.completeness}
    >
      <p className="flex flex-wrap items-center gap-1.5 text-2xs">
        <span
          aria-hidden
          className={`size-1.5 shrink-0 rounded-full ${dimensionDotClass(
            complete ? 'ready' : 'baseline',
          )}`}
        />
        <span className="font-medium text-text-secondary">Table growth · all stores</span>
        <span className="tabular text-text-primary">
          · {coverage.examined ?? 'unknown'} of {coverage.denominator ?? 'unknown'}{' '}
          {coverage.unit ?? 'stores'} fully compared
        </span>
      </p>
      {coverage.omission_reasons.length > 0 ? (
        <>
          <p className="mt-1.5 text-3xs font-medium uppercase tracking-wide text-text-muted">
            Stores without a complete per-table comparison
          </p>
          <ul className="mt-1 space-y-1 text-2xs text-text-muted">
            {coverage.omission_reasons.map((reason) => (
              <li key={reason}>{reason}</li>
            ))}
          </ul>
        </>
      ) : null}
    </section>
  );
}

function StoreCard({
  entry,
  tableGrowthThreshold,
}: {
  entry: StoreTelemetryEntryV1;
  tableGrowthThreshold: TableGrowthThresholdV1;
}) {
  // `observed` is a full page-level sample. `observed_bytes` is a real total
  // size with no page sample behind it, so free pages are UNKNOWN rather than
  // zero. Both have a size worth printing; only the sampled read knows how much
  // of that size is free, which is why the card still says which one it got.
  const sampled = entry.read.kind === 'observed';
  const sized = sampled || entry.read.kind === 'observed_bytes';
  return (
    <OverviewCard title={entry.store}>
      <div className="flex flex-col gap-2">
        <div className="flex flex-wrap items-center gap-2">
          <StateChip kind={readKindToState(entry.read.kind)} />
          <span className="text-2xs text-text-muted" data-store-roles={entry.roles.join(',')}>
            {storeRolesLabel(entry.roles)}
          </span>
        </div>
        {sized ? (
          <>
            <CapacityBar usedBytes={entry.total_bytes} freeBytes={entry.free_bytes} />
            <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs tabular">
              <dt className="text-text-muted">size</dt>
              <dd data-cell="numeric">{formatBytes(entry.total_bytes)}</dd>
              <dt className="text-text-muted">free pages</dt>
              <dd data-cell="numeric">{formatBytes(entry.free_bytes)}</dd>
              <dt className="text-text-muted">free ratio</dt>
              <dd data-cell="numeric">
                {entry.free_page_ratio != null
                  ? `${(entry.free_page_ratio * 100).toFixed(1)}%`
                  : '—'}
              </dd>
            </dl>
            {sampled ? null : (
              <p className="text-2xs text-text-muted">
                total size only · this store reported no page-level sample, so free pages are
                unmeasured rather than zero
              </p>
            )}
          </>
        ) : (
          <p className="text-xs text-text-muted">
            {readUnavailableMessage(entry.read)}
          </p>
        )}
        <DimensionRow label="Budget" presentation={budgetPresentation(entry.budget)} />
        <DimensionRow label="Growth" presentation={growthPresentation(entry.growth)} />
        <TableGrowthPanel
          growth={entry.table_growth}
          threshold={tableGrowthThreshold}
          store={entry.store}
        />
        <p className="truncate font-mono text-2xs text-text-muted" title={entry.path}>
          {entry.path}
        </p>
      </div>
    </OverviewCard>
  );
}

function TableGrowthPanel({
  growth,
  threshold,
  store,
}: {
  growth: TableGrowthDimensionV1;
  threshold: TableGrowthThresholdV1;
  store: string;
}) {
  const presentation = tableGrowthPresentation(growth);
  return (
    <section
      className="rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 p-2.5"
      data-table-growth-state={growth.state}
      data-table-growth-tone={presentation.tone}
      // Every store card carries one of these regions, so the store name is
      // part of the accessible name: a landmark list of identical "Per-table
      // growth" entries would name nothing.
      aria-label={`Per-table growth · ${store}`}
    >
      <div className="flex items-center gap-1.5 text-2xs">
        <span
          aria-hidden
          className={`size-1.5 shrink-0 rounded-full ${dimensionDotClass(presentation.tone)}`}
        />
        <span className="font-medium text-text-secondary">Table growth</span>
        <span className="text-text-primary">· {presentation.summary}</span>
      </div>

      {growth.state === 'observed' && growth.significant_samples.length > 0 ? (
        <ul className="mt-2 space-y-1.5" aria-label="Significant table growth samples">
          {growth.significant_samples.map((sample) => (
            <li
              key={`${sample.table}:${sample.previous_observed_at}:${sample.current_observed_at}`}
              className="rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 p-2 text-2xs"
              data-table-growth-sample={sample.table}
            >
              <p className="flex flex-wrap items-baseline justify-between gap-2">
                <code className="text-text-secondary">{sample.table}</code>
                <span className="tabular text-text-primary">
                  +{formatBytes(sample.growth_bytes)}
                </span>
              </p>
              <p className="mt-1 text-text-muted tabular">
                {formatBytes(sample.previous_bytes)} → {formatBytes(sample.current_bytes)}
              </p>
              <p className="mt-1 text-3xs text-text-muted">
                {formatMicrosUtc(sample.previous_observed_at)} →{' '}
                {formatMicrosUtc(sample.current_observed_at)}
              </p>
            </li>
          ))}
        </ul>
      ) : null}

      {growth.state === 'observed' && growth.omissions.length > 0 ? (
        <div className="mt-2">
          <p className="text-3xs font-medium uppercase tracking-wide text-text-muted">
            Omitted from significant list
          </p>
          <ul className="mt-1 space-y-1 text-2xs text-text-muted">
            {growth.omissions.map((omission) => {
              const omitted = tableGrowthOmissionPresentation(omission);
              return (
                <li
                  key={omitted.table}
                  className="flex flex-wrap items-baseline justify-between gap-x-2"
                  data-table-growth-omission={omitted.kind}
                >
                  <code className="text-text-secondary">{omitted.table}</code>
                  <span className="tabular">
                    {omitted.figure} · {omitted.detail}
                  </span>
                </li>
              );
            })}
          </ul>
        </div>
      ) : null}

      {/* Server reasons, verbatim, for every state, including an observed read
          whose table coverage is partial. The rows above format the structured
          byte evidence; these sentences say why each table was left out. */}
      {presentation.notes.length > 0 ? (
        <ul className="mt-2 space-y-1 text-2xs text-text-muted">
          {presentation.notes.map((note) => (
            <li key={note}>{note}</li>
          ))}
        </ul>
      ) : null}

      <p className="mt-2 text-3xs text-text-muted">
        Informational threshold · {formatBytes(threshold.absolute_bytes)} absolute, or{' '}
        {formatBytes(threshold.relative_floor_bytes)} and {threshold.relative_percent}% of previous
        size
      </p>
      <p className="mt-1 text-3xs text-text-muted">
        Coverage · this store · {growth.coverage.examined ?? 'unknown'} of{' '}
        {growth.coverage.denominator ?? 'unknown'} {growth.coverage.unit ?? 'reads'} compared
      </p>
    </section>
  );
}

/** One telemetry dimension. The state is carried by words (the summary names
 * it outright) with the tone dot as a redundant, never sole, signal. `unset`
 * also renders its owner setting as a mono token, so a missing setting is
 * distinct in structure and color from an undetermined read. */
function DimensionRow({
  label,
  presentation,
}: {
  label: string;
  presentation: DimensionPresentation;
}) {
  return (
    <div
      className="rounded-[var(--radius-chip)] bg-surface-2 px-2.5 py-2 text-2xs"
      data-dimension={label.toLowerCase()}
      data-dimension-state={presentation.state}
      data-dimension-tone={presentation.tone}
    >
      <p className="font-medium text-text-secondary">
        <span
          aria-hidden
          className={`mr-1.5 inline-block size-1.5 rounded-full align-middle ${dimensionDotClass(presentation.tone)}`}
        />
        {label} · <DimensionSummary presentation={presentation} />
      </p>
      {presentation.notes.map((note) => (
        <p key={note} className="mt-0.5 text-text-muted">
          {note}
        </p>
      ))}
    </div>
  );
}

/** The summary sentence, with a named owner setting rendered as a mono token.
 * The rendered text is unchanged, the mono run only makes "you have not set
 * this" structurally distinct from "the server could not tell". */
function DimensionSummary({ presentation }: { presentation: DimensionPresentation }) {
  const { settingKey, summary } = presentation;
  if (presentation.state !== 'unset' || !settingKey || !summary.endsWith(settingKey)) {
    return <span>{summary}</span>;
  }
  return (
    <span>
      {summary.slice(0, summary.length - settingKey.length)}
      <span className="font-mono" data-setting-key={settingKey}>
        {settingKey}
      </span>
    </span>
  );
}

export function ReadModelNotes({ notes }: { notes: string[] }) {
  return (
    <p className="border-t border-edge-subtle px-4 py-2 text-2xs text-text-muted">
      {notes.join(' · ')}
    </p>
  );
}

function readKindToState(kind: StorageTelemetryReadV1['kind']): DomainStateKind {
  switch (kind) {
    case 'observed':
      return 'ready';
    // A real measurement with less of it: a total with no page-level sample is
    // partial coverage, not a clean read and not a failed one.
    case 'observed_bytes':
      return 'partial';
    case 'unsupported':
      return 'unsupported';
    case 'denied':
      return 'denied';
    case 'unknown':
      return 'unknown';
    default:
      return assertNever(kind);
  }
}

function readUnavailableMessage(read: StorageTelemetryReadV1): string {
  switch (read.kind) {
    case 'observed':
    case 'observed_bytes':
      return '';
    case 'unsupported':
      return 'telemetry is unsupported for this store';
    case 'denied':
      return 'telemetry access was denied for this store';
    case 'unknown':
      return 'telemetry could not be determined for this store';
    default:
      return assertNever(read);
  }
}
