import { useRef, type KeyboardEvent, type ReactNode } from 'react';
import { ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight } from 'lucide-react';
import {
  assertNever,
  type DashboardEnvelopeV1,
  type LoomSessionRowV1,
  type LoomTemporalPayloadV1,
} from '../../contracts/generated.ts';
import { DataRow } from '../../ui/archetypes/ExplorerSplit.tsx';
import { cn } from '../../ui/cn';
import { toStripCoverage, toStripFreshness } from '../../ui/EnvelopeTruth.tsx';
import { EvidenceTruthStrip } from '../../ui/EvidenceTruthStrip.tsx';
import { formatStamp } from '../../ui/format.ts';
import { FigureRail, Panel } from '../../ui/instrument.tsx';
import { ReadSection, type ReadState } from '../../ui/ReadSection.tsx';
import { rovingRowsKeyDown } from '../../ui/rovingRows.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import { VirtualList } from '../../ui/VirtualList.tsx';
import {
  ROWS_PER_PAGE_OPTIONS,
  bucketKeyFor,
  pageBounds,
  pageOffset,
  recordedModels,
  sameSelection,
  sessionExtent,
  type RowsPerPage,
  type SessionExtent,
  type SessionSelection,
  type TimelineBucket,
} from './model.ts';

export interface SessionIndexProps {
  read: ReadState<DashboardEnvelopeV1<LoomTemporalPayloadV1>>;
  /** 1-based. */
  page: number;
  /** The route's `limit`. */
  rows: RowsPerPage;
  onPageChange: (page: number) => void;
  onRowsChange: (rows: RowsPerPage) => void;
  selection: SessionSelection | null;
  /** Clicking the selected row again deselects (null). */
  onSelect: (selection: SessionSelection | null) => void;
  /** Hover/focus on a row = inspect: report the row (or null when the
   * pointer/focus leaves). Never changes selection. */
  onInspectRow: (row: LoomSessionRowV1 | null) => void;
  /** Bucket key under inspection on the timeline; rows whose start bucket
   * (`bucketKeyFor(started_at, bucket)`) differs are visually dimmed. */
  inspectedBucket: string | null;
  bucket: TimelineBucket;
  /** Trailing content for the panel header (the page passes the transcript
   * search field). */
  actions?: ReactNode;
}

const PAGER_BEZEL =
  'inline-flex size-6 items-center justify-center border border-edge-subtle bg-surface-2 text-text-secondary group-hover:text-text-primary';

/** Column widths. The model yields first (below `lg`), then the extent (below
 * `md`), then the ordinal and the provider column (below `sm`, where the
 * provider moves onto the identity's second line); the identity and message
 * count survive 320px. */
const COLUMN = {
  ordinal: 'w-8 shrink-0 text-right max-sm:hidden',
  id: 'min-w-0 flex-1',
  provider: 'w-20 shrink-0 max-sm:hidden',
  model: 'w-36 shrink-0 max-lg:hidden',
  extent: 'w-56 shrink-0 max-md:hidden',
  messages: 'w-24 shrink-0 text-right',
} as const;

function ordinalText(value: number | null): string {
  return value === null ? '—' : String(value);
}

/** Start on the first line; what is known about the end on the second, so a
 * 44px row carries both stamps without truncating either. */
function extentLines(extent: SessionExtent): { start: string; end: string; startKnown: boolean } {
  switch (extent.kind) {
    case 'undated':
      return { start: 'start unrecorded', end: 'end unrecorded', startKnown: false };
    case 'ended':
      return {
        start: formatStamp(extent.start),
        end: `→ ${formatStamp(extent.end)} · ended`,
        startKnown: true,
      };
    case 'open':
      return {
        start: formatStamp(extent.start),
        end: `→ open · last ${formatStamp(extent.last)}`,
        startKnown: true,
      };
    case 'open_unobserved':
      return {
        start: formatStamp(extent.start),
        end: '→ open · no dated message after start',
        startKnown: true,
      };
    default:
      return assertNever(extent);
  }
}

function ExtentCell({ row }: { row: LoomSessionRowV1 }) {
  const lines = extentLines(sessionExtent(row));
  return (
    <>
      <span className={lines.startKnown ? undefined : 'text-text-muted'} title={lines.start}>
        {lines.start}
      </span>
      <span
        className={cn('text-3xs', lines.startKnown ? 'text-text-secondary' : 'text-text-muted')}
        title={lines.end}
      >
        {lines.end}
      </span>
    </>
  );
}

function ModelCell({ row }: { row: LoomSessionRowV1 }) {
  const { models, unrecorded } = recordedModels(row);
  if (models.length === 0) {
    return <span className="text-text-muted">model unrecorded</span>;
  }
  const joined = models.join(' · ');
  return (
    <span title={joined}>
      {joined}
      {unrecorded > 0 ? <span className="text-text-muted"> +{unrecorded} unrecorded</span> : null}
    </span>
  );
}

function ColumnHeader() {
  return (
    <div
      aria-hidden
      className="sticky top-0 z-10 flex h-7 items-center gap-3 border-b border-edge-subtle bg-surface-0/95 px-3 backdrop-blur"
    >
      <span className={cn('td-legend', COLUMN.ordinal)}>#</span>
      <span className={cn('td-legend truncate', COLUMN.id)}>Session ID · title · kind</span>
      <span className={cn('td-legend truncate', COLUMN.provider)}>Provider</span>
      <span className={cn('td-legend truncate', COLUMN.model)}>Model</span>
      <span className={cn('td-legend truncate', COLUMN.extent)}>Start → End</span>
      <span className={cn('td-legend truncate', COLUMN.messages)}>Messages</span>
    </div>
  );
}

function SessionRow({
  row,
  ordinal,
  pageMax,
  selected,
  dimmed,
  onSelect,
  onInspectRow,
}: {
  row: LoomSessionRowV1;
  ordinal: number;
  pageMax: number;
  selected: boolean;
  dimmed: boolean;
  onSelect: () => void;
  onInspectRow: (row: LoomSessionRowV1 | null) => void;
}) {
  return (
    <div
      data-session-row={row.session_id}
      data-session-provider={row.provider}
      className={cn(dimmed && 'opacity-40 transition-opacity duration-[var(--dur-state)]')}
      onMouseEnter={() => onInspectRow(row)}
      onMouseLeave={() => onInspectRow(null)}
      onFocus={() => onInspectRow(row)}
      onBlur={() => onInspectRow(null)}
    >
      <DataRow selected={selected} onSelect={onSelect}>
        <span
          className={cn('td-value text-3xs text-text-muted', COLUMN.ordinal)}
          data-cell="numeric"
        >
          {ordinal}
        </span>
        <span className={cn('flex flex-col gap-0.5', COLUMN.id)}>
          <span className="td-value truncate text-xs text-text-primary" title={row.session_id}>
            {row.session_id}
          </span>
          <span className="flex min-w-0 items-baseline gap-1.5">
            <span className="td-legend shrink-0 text-text-secondary sm:hidden">{row.provider}</span>
            <span
              className={cn('truncate text-3xs', row.title ? 'text-text-muted' : 'italic text-text-muted')}
              title={row.title ?? undefined}
            >
              {row.title ?? 'untitled'}
            </span>
            {row.is_subagent ? (
              <span className="td-legend shrink-0 border border-edge-subtle px-1 text-text-secondary">
                subagent
              </span>
            ) : null}
          </span>
        </span>
        <span
          className={cn(
            'td-legend truncate border border-edge-subtle px-1 py-1 text-center',
            COLUMN.provider,
          )}
          title={row.provider}
        >
          {row.provider}
        </span>
        <span className={cn('td-value truncate text-2xs text-text-secondary', COLUMN.model)}>
          <ModelCell row={row} />
        </span>
        <span
          className={cn('td-value flex flex-col gap-0.5 text-2xs [&>span]:truncate', COLUMN.extent)}
          data-cell="extent"
        >
          <ExtentCell row={row} />
        </span>
        <FigureRail
          value={String(row.messages)}
          unit="msg"
          width="wide"
          fraction={pageMax > 0 ? row.messages / pageMax : null}
        />
      </DataRow>
    </div>
  );
}

function PagerButton({
  label,
  disabled,
  onClick,
  children,
}: {
  label: string;
  disabled: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className="td-hit group disabled:opacity-40"
      aria-label={label}
      disabled={disabled}
      onClick={onClick}
    >
      <span className={PAGER_BEZEL}>{children}</span>
    </button>
  );
}

export function SessionIndex({
  read,
  page,
  rows,
  onPageChange,
  onRowsChange,
  selection,
  onSelect,
  onInspectRow,
  inspectedBucket,
  bucket,
  actions,
}: SessionIndexProps): ReactNode {
  const rowsRef = useRef<HTMLDivElement | null>(null);
  const onRowsKeyDown = (event: KeyboardEvent) => rovingRowsKeyDown(rowsRef.current, event);

  const envelope = read.kind === 'ready' ? read.value : null;
  const payload = envelope?.payload ?? null;
  const store = payload !== null && payload.available ? payload : null;
  const sessions = store?.sessions ?? [];
  const total = store?.total ?? null;
  const { first, last, pageCount } = pageBounds(page, rows, sessions.length, total);
  const offset = pageOffset(page, rows);
  const pageMax = sessions.reduce((max, row) => Math.max(max, row.messages), 0);

  const atFirst = page <= 1;
  const atLast = pageCount !== null ? page >= pageCount : sessions.length !== rows;

  let body: ReactNode;
  if (read.kind === 'blocked') {
    body = (
      <ReadSection title="Sessions" chrome="centered" state={read}>
        {() => null}
      </ReadSection>
    );
  } else if (store === null) {
    body = (
      <div className="flex flex-col items-start gap-2 p-3">
        <StateChip kind="unknown" detail="session store not readable" />
        <p className="text-xs text-text-muted">
          The daemon answered but reported its session store unavailable; there is no index to
          page.
        </p>
      </div>
    );
  } else if (sessions.length === 0 && store.total === 0) {
    body = (
      <div className="flex flex-col items-start gap-2 p-3">
        <StateChip kind="complete_zero_findings" detail="the session store holds no sessions" />
      </div>
    );
  } else if (sessions.length === 0) {
    const lastPage = pageCount ?? 1;
    body = (
      <div className="flex flex-col items-start gap-2 p-3">
        <StateChip kind="partial" detail={`page ${page} is past the last page (${lastPage})`} />
        <button
          type="button"
          className="td-hit group"
          onClick={() => onPageChange(lastPage)}
        >
          <span className="inline-flex items-center gap-1 border border-edge-subtle bg-surface-2 px-2 py-1 text-3xs text-text-secondary group-hover:text-text-primary">
            Go to last page
          </span>
        </button>
      </div>
    );
  } else {
    body = (
      <div ref={rowsRef} onKeyDown={onRowsKeyDown} className="min-h-0 flex-1 overflow-auto">
        <VirtualList
          items={sessions}
          getKey={(row) => `${row.provider}\u0000${row.session_id}`}
          header={<ColumnHeader />}
          renderItem={(row, index) => {
            const rowSelection: SessionSelection = {
              provider: row.provider,
              sessionId: row.session_id,
            };
            const selected = sameSelection(selection, rowSelection);
            const dimmed =
              inspectedBucket !== null &&
              (row.started_at === null || bucketKeyFor(row.started_at, bucket) !== inspectedBucket);
            return (
              <SessionRow
                row={row}
                ordinal={offset + index + 1}
                pageMax={pageMax}
                selected={selected}
                dimmed={dimmed}
                onSelect={() => onSelect(selected ? null : rowSelection)}
                onInspectRow={onInspectRow}
              />
            );
          }}
        />
      </div>
    );
  }

  const footer = (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
      <nav aria-label="Session pages" className="flex items-center gap-1">
        <PagerButton label="First page" disabled={atFirst} onClick={() => onPageChange(1)}>
          <ChevronsLeft aria-hidden size={12} />
        </PagerButton>
        <PagerButton label="Previous page" disabled={atFirst} onClick={() => onPageChange(page - 1)}>
          <ChevronLeft aria-hidden size={12} />
        </PagerButton>
        <span role="status" className="td-value px-1 text-3xs text-text-muted" data-cell="numeric">
          {`Page ${page} of ${pageCount ?? '?'}`}
        </span>
        <PagerButton label="Next page" disabled={atLast} onClick={() => onPageChange(page + 1)}>
          <ChevronRight aria-hidden size={12} />
        </PagerButton>
        <PagerButton
          label="Last page"
          disabled={pageCount === null || page >= pageCount}
          onClick={() => {
            if (pageCount !== null) onPageChange(pageCount);
          }}
        >
          <ChevronsRight aria-hidden size={12} />
        </PagerButton>
      </nav>
      <label className="td-legend flex items-center gap-1.5">
        rows per page
        <select
          className="td-value min-h-[var(--touch-target-min)] border border-edge-subtle bg-surface-2 px-1.5 text-2xs"
          value={rows}
          onChange={(event) => onRowsChange(Number(event.target.value) as RowsPerPage)}
        >
          {ROWS_PER_PAGE_OPTIONS.map((option) => (
            <option key={option} value={option}>
              {option}
            </option>
          ))}
        </select>
      </label>
      {envelope ? (
        <>
          <EvidenceTruthStrip
            coverage={toStripCoverage(envelope.coverage)}
            freshness={toStripFreshness(envelope.freshness)}
            omissions={envelope.coverage.omitted ?? undefined}
          />
          {envelope.coverage.omission_reasons.map((reason) => (
            <span key={reason} className="text-3xs text-text-muted">
              {reason}
            </span>
          ))}
        </>
      ) : null}
      {store !== null && sessions.length > 0 ? (
        <span className="text-3xs text-text-muted">
          message rails are scaled to the heaviest session on this page ({pageMax} msg)
        </span>
      ) : null}
      {payload ? (
        <StateChip
          kind={payload.temporal_refresh.state}
          detail={`temporal refresh · ${payload.temporal_refresh.active_generations} active generations`}
        />
      ) : null}
    </div>
  );

  return (
    <Panel
      legend="Sessions"
      elevation="well"
      // Grows into whatever height the aperture column leaves after the
      // timeline and rows scroll inside; the floor keeps a short viewport from
      // collapsing the rows to nothing (the column scrolls instead).
      className="min-h-[var(--pane-min-height)] flex-1"
      bodyClassName="flex min-h-0 flex-col p-0"
      actions={
        <>
          <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
            {`${ordinalText(first)}–${ordinalText(last)} of ${total === null ? '—' : total.toLocaleString()}`}
          </span>
          {actions}
        </>
      }
      footer={footer}
    >
      {body}
    </Panel>
  );
}
