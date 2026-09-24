import {
  useMemo,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
  type SyntheticEvent,
} from 'react';
import {
  assertNever,
  type DashboardEnvelopeV1,
  type LcmTimelineBucketV1,
  type LcmTimelinePayloadV1,
} from '../../contracts/generated.ts';
import { cn } from '../../ui/cn.ts';
import { toStripCoverage, toStripFreshness } from '../../ui/EnvelopeTruth.tsx';
import { EvidenceTruthStrip } from '../../ui/EvidenceTruthStrip.tsx';
import { formatCount } from '../../ui/format.ts';
import { Panel } from '../../ui/instrument.tsx';
import { ReadSection, type ReadState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import {
  bucketStart,
  provenanceTally,
  TIMELINE_WINDOW_OPTIONS,
  type TimelineBucket,
  type TimelineWindow,
} from './model.ts';
import { tokenCountLabel } from './tokenLabel.ts';

export interface VolumeTimelineProps {
  /** The resolved timeline read. Blocked states are rendered here (see States). */
  read: ReadState<DashboardEnvelopeV1<LcmTimelinePayloadV1>>;
  bucket: TimelineBucket;
  window: TimelineWindow;
  onBucketChange: (bucket: TimelineBucket) => void;
  onWindowChange: (window: TimelineWindow) => void;
  /** Bucket key the index is inspecting (a hovered/focused session row's start bucket). Draw a cyan focus marker on that bucket. Never changes data. */
  inspectedBucket: string | null;
  /** Fires with the bucket key under pointer hover or keyboard focus inside the field, and null when none. The page dims index rows outside it. */
  onInspectBucket: (key: string | null) => void;
}

const TITLE = 'Message volume timeline';
const FIELD_HEIGHT = 100;
/** Narrow enough that the gap between neighbours survives 2000 columns. */
const COLUMN_WIDTH = 0.72;
const MAX_Y_TICKS = 4;

/** Hour labels carry a time and are twice as wide as day labels. */
function xLabelTarget(bucket: TimelineBucket): number {
  switch (bucket) {
    case 'day':
      return 8;
    case 'hour':
      return 5;
    default:
      return assertNever(bucket);
  }
}

const BUCKET_OPTIONS: readonly { value: TimelineBucket; label: string }[] = [
  { value: 'day', label: 'DAILY' },
  { value: 'hour', label: 'HOURLY' },
];

function unitWord(bucket: TimelineBucket): string {
  switch (bucket) {
    case 'day':
      return 'days';
    case 'hour':
      return 'hours';
    default:
      return assertNever(bucket);
  }
}

function axisLabel(key: string, bucket: TimelineBucket): string {
  const start = bucketStart(key);
  if (start === null) return key;
  const date = new Date(start * 1000);
  const day = date.toLocaleDateString('en-GB', { day: '2-digit', month: 'short', timeZone: 'UTC' });
  switch (bucket) {
    case 'day':
      return day;
    case 'hour':
      return `${day} ${String(date.getUTCHours()).padStart(2, '0')}:00`;
    default:
      return assertNever(bucket);
  }
}

function logHeight(count: number, max: number): number {
  if (max <= 0 || count <= 0) return 0;
  return (Math.log10(1 + count) / Math.log10(1 + max)) * FIELD_HEIGHT;
}

function pickEvenly<T>(items: readonly T[], limit: number): T[] {
  if (items.length <= limit) return [...items];
  const step = (items.length - 1) / (limit - 1);
  return Array.from({ length: limit }, (_, i) => items[Math.round(i * step)]!);
}

function powerTicks(max: number): number[] {
  const ticks: number[] = [];
  for (let value = 1; value <= max; value *= 10) ticks.push(value);
  return pickEvenly(ticks, MAX_Y_TICKS);
}

function xLabelIndices(count: number, target: number): number[] {
  const step = Math.max(1, Math.ceil(count / target));
  const indices: number[] = [];
  for (let i = 0; i < count; i += step) indices.push(i);
  return indices;
}

function compareKeys(left: LcmTimelineBucketV1, right: LcmTimelineBucketV1): number {
  if (left.bucket < right.bucket) return -1;
  if (left.bucket > right.bucket) return 1;
  return 0;
}

function Segmented<T extends string | number>({
  label,
  options,
  value,
  onChange,
}: {
  label: string;
  options: readonly { value: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="flex items-center">
      {options.map((option) => {
        const checked = option.value === value;
        return (
          <button
            key={String(option.value)}
            type="button"
            role="radio"
            aria-checked={checked}
            className="td-hit"
            onClick={() => onChange(option.value)}
          >
            <span
              className={cn(
                'inline-flex h-5 items-center border border-edge-subtle bg-surface-2 px-1.5 text-3xs uppercase tracking-[0.1em]',
                checked
                  ? 'border-b-2 border-b-accent text-text-primary'
                  : 'text-text-muted hover:text-text-secondary',
              )}
            >
              {option.label}
            </span>
          </button>
        );
      })}
    </div>
  );
}

export function VolumeTimeline({
  read,
  bucket,
  window,
  onBucketChange,
  onWindowChange,
  inspectedBucket,
  onInspectBucket,
}: VolumeTimelineProps): ReactNode {
  const controls = (
    <div className="flex flex-wrap items-center gap-x-2 gap-y-1 max-md:w-full md:shrink-0">
      <Segmented label="Bucket" options={BUCKET_OPTIONS} value={bucket} onChange={onBucketChange} />
      <span className="td-legend">dated buckets</span>
      <Segmented
        label="Dated buckets loaded"
        options={TIMELINE_WINDOW_OPTIONS.map((option) => ({ value: option, label: String(option) }))}
        value={window}
        onChange={onWindowChange}
      />
    </div>
  );

  const footer =
    read.kind === 'ready' ? <TimelineFooter envelope={read.value} /> : undefined;

  return (
    <Panel
      legend={TITLE}
      actions={controls}
      // Six options do not fit beside the legend below `md`; the header wraps
      // there instead of clipping the window controls off the edge.
      headerClassName="max-md:h-auto max-md:flex-wrap max-md:gap-y-1 max-md:py-1"
      elevation="well"
      footer={footer}
    >
      <ReadSection title={TITLE} chrome="centered" state={read}>
        {(envelope) => (
          <TimelineBody
            payload={envelope.payload}
            bucket={bucket}
            inspectedBucket={inspectedBucket}
            onInspectBucket={onInspectBucket}
          />
        )}
      </ReadSection>
    </Panel>
  );
}

function TimelineBody({
  payload,
  bucket,
  inspectedBucket,
  onInspectBucket,
}: {
  payload: LcmTimelinePayloadV1;
  bucket: TimelineBucket;
  inspectedBucket: string | null;
  onInspectBucket: (key: string | null) => void;
}) {
  if (payload.exists === false) {
    return (
      <div className="flex min-h-28 items-center justify-center">
        <StateChip
          kind="unknown"
          detail="LCM session store is unavailable; message volume is unknown"
        />
      </div>
    );
  }
  if (payload.buckets.length === 0) {
    const undated = payload.undated.count;
    return (
      <div className="flex min-h-28 items-center justify-center">
        {undated > 0 ? (
          <StateChip
            kind="partial"
            detail={`${undated.toLocaleString()} undated messages only; nothing can be placed on the axis`}
          />
        ) : (
          <StateChip
            kind="complete_zero_findings"
            detail="no dated messages in the loaded window"
          />
        )}
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <VolumeField
        payload={payload}
        bucket={bucket}
        inspectedBucket={inspectedBucket}
        onInspectBucket={onInspectBucket}
      />
      <ExactBuckets buckets={payload.buckets} />
    </div>
  );
}

function VolumeField({
  payload,
  bucket,
  inspectedBucket,
  onInspectBucket,
}: {
  payload: LcmTimelinePayloadV1;
  bucket: TimelineBucket;
  inspectedBucket: string | null;
  onInspectBucket: (key: string | null) => void;
}) {
  // Sorted oldest→newest so "last" is "newest" whatever `coverage.ordering`
  // the route answered with; the keys are ISO-prefixed and sort as strings.
  const buckets = useMemo(() => [...payload.buckets].sort(compareKeys), [payload.buckets]);
  const count = buckets.length;
  const max = useMemo(() => buckets.reduce((m, b) => Math.max(m, b.count), 0), [buckets]);
  const tally = provenanceTally(buckets, payload.undated);
  const units = unitWord(bucket);

  // Keyed by bucket rather than by index so a re-read that drops the bucket
  // drops the focus with it instead of landing on a neighbour.
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const activeIndex = activeKey === null ? -1 : buckets.findIndex((b) => b.bucket === activeKey);
  const active = activeIndex >= 0 ? buckets[activeIndex]! : null;

  const activate = (key: string | null) => {
    if (key === activeKey) return;
    setActiveKey(key);
    onInspectBucket(key);
  };

  const onMouseMove = (event: MouseEvent<SVGSVGElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    if (rect.width <= 0) return;
    const ratio = (event.clientX - rect.left) / rect.width;
    const index = Math.min(count - 1, Math.max(0, Math.floor(ratio * count)));
    activate(buckets[index]?.bucket ?? null);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const current = activeIndex >= 0 ? activeIndex : null;
    let next: number | null;
    switch (event.key) {
      case 'ArrowLeft':
        next = current === null ? count - 1 : Math.max(0, current - 1);
        break;
      case 'ArrowRight':
        next = current === null ? count - 1 : Math.min(count - 1, current + 1);
        break;
      case 'Home':
        next = 0;
        break;
      case 'End':
        next = count - 1;
        break;
      case 'Escape':
        next = null;
        break;
      default:
        return;
    }
    event.preventDefault();
    activate(next === null ? null : (buckets[next]?.bucket ?? null));
  };

  const newest = buckets[count - 1]!;
  const inspectedIndex =
    inspectedBucket === null ? -1 : buckets.findIndex((b) => b.bucket === inspectedBucket);
  const returned = payload.coverage?.returned_buckets ?? count;
  const totalDated = payload.coverage?.total_dated_buckets ?? count;

  const status = active
    ? [
        active.bucket,
        `${active.count.toLocaleString()} messages`,
        tokenCountLabel(active.token_count, active.token_count_provenance) ??
          'token count unavailable',
        active.unknown_message_count > 0
          ? `${active.unknown_message_count.toLocaleString()} unknown token counts`
          : null,
      ]
        .filter((part): part is string => part !== null)
        .join(' · ')
    : `${returned.toLocaleString()} of ${totalDated.toLocaleString()} dated ${units} loaded · ${tally.dated.toLocaleString()} messages · newest ${newest.bucket}`;

  return (
    <div
      tabIndex={0}
      role="group"
      aria-label="Message volume field"
      className="flex flex-col gap-1.5"
      onKeyDown={onKeyDown}
      onFocus={() => {
        if (active === null) activate(newest.bucket);
      }}
      onBlur={() => activate(null)}
    >
      <div className="relative pl-9">
        <div aria-hidden className="absolute inset-y-0 left-0 w-8">
          {powerTicks(max).map((value) => (
            <span
              key={value}
              className="td-value absolute right-1 translate-y-1/2 text-xs leading-none text-text-muted"
              style={{ bottom: `${logHeight(value, max)}%` }}
              data-cell="numeric"
            >
              {formatCount(value, 1000)}
            </span>
          ))}
        </div>
        <svg
          role="img"
          aria-label={`Message volume over ${count.toLocaleString()} dated ${units}, ${formatCount(tally.dated)} messages`}
          viewBox={`0 0 ${count} ${FIELD_HEIGHT}`}
          preserveAspectRatio="none"
          className="h-32 w-full"
          shapeRendering="crispEdges"
          onMouseMove={onMouseMove}
          onMouseLeave={() => activate(null)}
        >
          {inspectedIndex >= 0 ? (
            <rect
              data-inspected-bucket={inspectedBucket ?? undefined}
              x={inspectedIndex}
              y={0}
              width={COLUMN_WIDTH}
              height={FIELD_HEIGHT}
              fill="var(--raw-accent)"
              opacity={0.35}
            />
          ) : null}
          {buckets.map((row, index) => {
            const total = logHeight(row.count, max);
            const knownShare = row.count > 0 ? row.known_message_count / row.count : 0;
            const knownHeight = total * knownShare;
            const unknownHeight = total - knownHeight;
            const isActive = index === activeIndex;
            return (
              <g
                key={row.bucket}
                data-bucket={row.bucket}
                data-active={isActive ? 'true' : undefined}
                className={cn(
                  'transition-opacity duration-[var(--dur-state)]',
                  active !== null && !isActive && 'opacity-35',
                )}
              >
                {row.count === 0 ? (
                  <rect
                    x={index}
                    y={FIELD_HEIGHT - 1}
                    width={COLUMN_WIDTH}
                    height={1}
                    fill="var(--raw-edge-strong)"
                  />
                ) : (
                  <>
                    {knownHeight > 0 ? (
                      <rect
                        x={index}
                        y={FIELD_HEIGHT - knownHeight}
                        width={COLUMN_WIDTH}
                        height={knownHeight}
                        fill="var(--raw-accent)"
                      />
                    ) : null}
                    {unknownHeight > 0 ? (
                      <rect
                        x={index}
                        y={FIELD_HEIGHT - total}
                        width={COLUMN_WIDTH}
                        height={unknownHeight}
                        fill="var(--raw-alert)"
                      />
                    ) : null}
                  </>
                )}
              </g>
            );
          })}
        </svg>
      </div>
      <div aria-hidden className="relative ml-9 h-4">
        {xLabelIndices(count, xLabelTarget(bucket)).map((index, position) => (
          <span
            key={index}
            className={cn(
              'td-value absolute top-0 -translate-x-1/2 whitespace-nowrap text-xs leading-none text-text-muted',
              // Every other label yields below `md`, where the field is too
              // narrow to hold them all; the exact-bucket table keeps every key.
              position % 2 === 1 && 'max-md:hidden',
            )}
            style={{ left: `${((index + COLUMN_WIDTH / 2) / count) * 100}%` }}
          >
            {axisLabel(buckets[index]!.bucket, bucket)}
          </span>
        ))}
      </div>
      <p role="status" className="td-value text-sm text-text-secondary tabular">
        {status}
      </p>
      <div className="td-legend flex flex-wrap items-center gap-x-3 gap-y-1 whitespace-normal">
        <span className="inline-flex items-center gap-1">
          <span aria-hidden className="inline-block size-2 shrink-0 bg-accent" />
          counted tokens (o200k approximate)
        </span>
        <span className="inline-flex items-center gap-1">
          <span aria-hidden className="inline-block size-2 shrink-0 bg-alert" />
          token count unavailable
        </span>
        <span>log scale · UTC buckets</span>
      </div>
    </div>
  );
}

function ExactBuckets({ buckets }: { buckets: readonly LcmTimelineBucketV1[] }) {
  const [open, setOpen] = useState(false);
  const rows = useMemo(() => [...buckets].sort(compareKeys), [buckets]);
  return (
    <details onToggle={(event: SyntheticEvent<HTMLDetailsElement>) => setOpen(event.currentTarget.open)}>
      <summary className="min-h-[var(--touch-target-min)] content-center td-legend cursor-pointer">
        Exact buckets ({buckets.length.toLocaleString()})
      </summary>
      {open ? (
        <table className="w-full border-collapse text-left">
          <thead>
            <tr className="border-b border-edge-subtle">
              <th scope="col" className="td-legend py-1 pr-2 font-normal">
                bucket
              </th>
              <th scope="col" className="td-legend py-1 pr-2 text-right font-normal">
                messages
              </th>
              <th scope="col" className="td-legend py-1 pr-2 text-right font-normal">
                counted
              </th>
              <th scope="col" className="td-legend py-1 pr-2 text-right font-normal">
                unknown
              </th>
              <th scope="col" className="td-legend py-1 font-normal">
                tokens
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.bucket} data-bucket-row={row.bucket} className="border-b border-edge-subtle">
                <td className="td-value py-0.5 pr-2 text-sm text-text-primary">{row.bucket}</td>
                <td className="td-value py-0.5 pr-2 text-right text-sm" data-cell="numeric">
                  {row.count.toLocaleString()}
                </td>
                <td className="td-value py-0.5 pr-2 text-right text-sm" data-cell="numeric">
                  {row.known_message_count.toLocaleString()}
                </td>
                <td className="td-value py-0.5 pr-2 text-right text-sm" data-cell="numeric">
                  {row.unknown_message_count.toLocaleString()}
                </td>
                <td className="td-value py-0.5 text-sm text-text-muted">
                  {tokenCountLabel(row.token_count, row.token_count_provenance) ??
                    'token count unavailable'}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
    </details>
  );
}

function TimelineFooter({ envelope }: { envelope: DashboardEnvelopeV1<LcmTimelinePayloadV1> }) {
  const { coverage: timelineCoverage, undated } = envelope.payload;
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-text-muted">
      {timelineCoverage ? (
        <span>
          {timelineCoverage.returned_buckets.toLocaleString()} of{' '}
          {timelineCoverage.total_dated_buckets.toLocaleString()} dated buckets loaded (limit{' '}
          {timelineCoverage.limit.toLocaleString()})
          {timelineCoverage.truncated ? '; older buckets omitted' : ''}
        </span>
      ) : (
        <span>Timeline coverage was not reported.</span>
      )}
      {undated.count > 0 ? (
        <span>
          {undated.count.toLocaleString()} undated messages are held separately from this field
        </span>
      ) : null}
      <EvidenceTruthStrip
        coverage={toStripCoverage(envelope.coverage)}
        freshness={toStripFreshness(envelope.freshness)}
        omissions={envelope.coverage.omitted ?? undefined}
      />
      {envelope.coverage.omission_reasons.map((reason) => (
        <span key={reason} className="text-text-secondary">
          {reason}
        </span>
      ))}
    </div>
  );
}
