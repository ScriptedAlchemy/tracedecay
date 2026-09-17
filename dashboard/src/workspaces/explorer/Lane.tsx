/**
 * One Explorer lane: a single authority's answer, drawn as its own column.
 *
 * Four of these sit abreast, and the whole design is that they are
 * independent. Each carries its own header state, its own headline count,
 * its own rows and its own footer, so a ready lane beside an unavailable one
 * reads as two facts about two authorities — never as one search that mostly
 * worked. Nothing in a lane is computed from another lane.
 */
import { type KeyboardEvent, type ReactNode } from 'react';
import { EvidencePattern } from '../../ui/EvidencePattern.tsx';
import { StateChip } from '../../ui/StateChip';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { Highlight } from '../../ui/search/Highlight.tsx';
import { cn } from '../../ui/cn';
import { Corners, Meter } from '../../ui/instrument.tsx';
import { compactRelativeAge } from '../../ui/time.ts';
import { laneGrade } from './evidence.ts';
import { LANE_BY_ID, LANE_ICON } from './laneChrome.ts';
import {
  laneEvidence,
  laneHits,
  laneStateDetail,
  laneStateKind,
  type ExplorerLaneReadModel,
} from './laneModel.ts';
import type { Hit } from './model.ts';

/** Fixed row height shared with the virtualizer: title, location, meta. */
export const LANE_ROW_HEIGHT = 60;

/** A code-graph generation watermark is a long dotted digest; the footer
 * shows enough to tell two apart and carries the whole value as a title. */
function clipWatermark(watermark: string): string {
  return watermark.length <= 28 ? watermark : `${watermark.slice(0, 27)}…`;
}

/** Marks each lane's row region so the grid can traverse lanes with the
 * horizontal arrows without knowing how a lane lays itself out. */
export const LANE_LIST_ATTR = 'data-lane-list';

export function Lane({
  read,
  rows,
  terms,
  searching,
  query,
  selectedKey,
  onSelect,
  onPeek,
}: {
  read: ExplorerLaneReadModel;
  /** The lane's rows after the pivot. Always empty for a lane without rows. */
  rows: readonly Hit[];
  terms: readonly string[];
  searching: boolean;
  query: string;
  selectedKey: string | undefined;
  onSelect: (hit: Hit) => void;
  onPeek: (hit: Hit | null) => void;
}) {
  const spec = LANE_BY_ID[read.lane];
  const Icon = LANE_ICON[read.lane];
  const kind = laneStateKind(read);
  const grade = laneGrade(read);
  const answered = read.state === 'ready' || read.state === 'partial';
  const loaded = laneHits(read).length;
  const total = answered ? read.reportedTotal : null;
  const share = read.state === 'ready' && total != null && total > 0 ? loaded / total : null;
  const listLabel = `${spec.label} results`;

  return (
    <section
      aria-label={`${spec.label} lane`}
      data-lane={read.lane}
      data-lane-state={read.state}
      className="relative flex min-h-[var(--pane-min-height)] min-w-0 flex-col border border-edge-subtle bg-surface-1"
    >
      <Corners tone={read.state === 'ready' ? 'signal' : 'edge'} />
      {/* Name over state, so four headers align whatever length a state's
        * detail runs to. The chip truncates its detail; the body repeats the
        * full sentence for any lane that has no rows to show instead. */}
      <header className="flex shrink-0 flex-col gap-1 border-b border-edge-subtle px-2.5 py-1.5">
        <span className="flex h-5 items-center gap-2">
          <span aria-hidden className={cn('h-3 w-[3px] shrink-0', spec.railClass)} />
          <h2 className="td-title text-text-primary">{spec.label}</h2>
          <span aria-hidden className="td-rule" />
          <Icon aria-hidden size={14} className={cn('shrink-0', spec.textClass)} />
        </span>
        <StateChip
          kind={kind}
          detail={laneStateDetail(read)}
          className="w-fit max-w-full [&>span:last-child]:truncate"
        />
      </header>

      {/* The headline count, or the absence of one. A lane that did not
        * answer prints a dash, never a zero: zero is what a source says when
        * it looked and found nothing. */}
      <div className="flex shrink-0 flex-col gap-1 border-b border-edge-subtle px-2.5 py-1.5">
        <div className="flex items-baseline justify-between gap-2">
          <span className="td-legend truncate">{spec.countLabel}</span>
          <span className="td-display text-lg" data-cell="numeric">
            {answered ? loaded.toLocaleString() : '—'}
          </span>
        </div>
        <span className="flex items-center justify-between gap-2 text-3xs text-text-muted">
          <span className="flex min-w-0 items-center gap-1.5">
            {/* How well the count is known, on the shared pattern axis: solid
              * when the source reported a real denominator, hatched when rows
              * arrived without one. Distinct from the record grade beside it. */}
            {answered ? (
              <EvidencePattern quality={laneEvidence(read)} className="shrink-0 text-3xs" />
            ) : null}
            <span className="truncate">
              {answered
                ? read.state === 'partial'
                  ? 'loaded from an incomplete read; records were omitted'
                  : total != null
                    ? `loaded of ${total.toLocaleString()} matching reported`
                    : searching
                      ? 'loaded; no source total reported'
                      : 'shown from the overview endpoint'
                : 'no count reported'}
            </span>
          </span>
          {grade !== null ? (
            <span
              className="td-legend shrink-0 text-text-secondary"
              data-evidence-grade={grade}
            >
              {grade}
            </span>
          ) : null}
        </span>
        {share !== null ? <Meter fraction={share} className="w-full" tone="bg-accent/80" /> : null}
        {answered && read.unreadableRows > 0 ? (
          <span className="text-3xs text-state-partial">
            {read.unreadableRows.toLocaleString()} returned rows could not be read
          </span>
        ) : null}
      </div>

      <LaneBody
        read={read}
        rows={rows}
        terms={terms}
        searching={searching}
        query={query}
        listLabel={listLabel}
        selectedKey={selectedKey}
        onSelect={onSelect}
        onPeek={onPeek}
      />

      <footer className="flex shrink-0 flex-wrap items-center justify-between gap-x-2 gap-y-0.5 border-t border-edge-subtle px-2.5 py-1 text-3xs text-text-muted">
        <span className="td-value text-3xs text-text-muted">
          {answered ? `${rows.length.toLocaleString()} shown` : '—'}
          {answered && read.hasMore === true ? ' · more rows remain past this page' : ''}
          {/* The source's own freshness word and watermark, so a served page
            * says how current it is rather than looking current by default. */}
          {answered ? (
            <span data-lane-freshness={read.freshness} title={read.watermark ?? undefined}>
              {` · ${read.freshness}`}
              {read.watermark !== null ? ` @ ${clipWatermark(read.watermark)}` : ''}
            </span>
          ) : null}
        </span>
        <span>
          {answered
            ? 'source order · no cross-lane rank'
            : read.state === 'pending'
              ? 'reading'
              : searching
                ? 'not served'
                : 'no rows'}
        </span>
      </footer>
    </section>
  );
}

/** The lane's body: rows when it has them, otherwise the exact condition that
 * explains why it does not. Every branch is a distinct sentence; an empty
 * well without one would be the lane implying an answer it never gave. */
function LaneBody({
  read,
  rows,
  terms,
  searching,
  query,
  listLabel,
  selectedKey,
  onSelect,
  onPeek,
}: {
  read: ExplorerLaneReadModel;
  rows: readonly Hit[];
  terms: readonly string[];
  searching: boolean;
  query: string;
  listLabel: string;
  selectedKey: string | undefined;
  onSelect: (hit: Hit) => void;
  onPeek: (hit: Hit | null) => void;
}) {
  const spec = LANE_BY_ID[read.lane];
  const answered = read.state === 'ready' || read.state === 'partial';

  if (answered && rows.length > 0) {
    return (
      <div
        role="region"
        aria-label={listLabel}
        tabIndex={0}
        {...{ [LANE_LIST_ATTR]: read.lane }}
        onKeyDown={rovingRows}
        className={cn(
          'td-well min-h-0 flex-1 overflow-auto',
          // Hover inspects: the row under the pointer holds full strength and
          // the rest of the lane steps back. Opacity only, so a dimmed row
          // keeps its text contrast and nothing is hidden.
          '[&:hover_button:not(:hover)]:opacity-70',
        )}
      >
        <VirtualList
          items={[...rows]}
          estimateHeight={LANE_ROW_HEIGHT}
          getKey={(hit) => hit.key}
          renderItem={(hit) => (
            <LaneRow
              hit={hit}
              terms={terms}
              selected={selectedKey === hit.key}
              onSelect={() => onSelect(hit)}
              onPeek={onPeek}
            />
          )}
        />
      </div>
    );
  }

  const condition = ((): { title: string; body: string } => {
    switch (read.state) {
      case 'pending':
        return {
          title: read.phase === 'queued' ? 'Queued' : 'Reading',
          body: searching
            ? `${spec.label} is ${read.phase ?? 'reading'}; nothing is shown until it answers.`
            : `Loading ${spec.browseLabel}.`,
        };
      case 'ready':
        return laneHits(read).length === 0
          ? {
              title: 'Served empty',
              body: searching
                ? `${spec.label} answered and found no rows for “${query}”. This is the source's own answer, not a gap.`
                : `The overview answered with no rows: nothing is indexed here yet.`,
            }
          : {
              title: 'Nothing under this pivot',
              body: 'The pivot is applied to the rows loaded, not to the whole index. Clear it to see them.',
            };
      case 'partial':
        return laneHits(read).length === 0
          ? {
              title: 'Answered with omissions, none shown',
              body: `${spec.label} reported omitted records and returned no rows this surface could show.${read.detail ? ` ${read.detail}` : ''}`,
            }
          : {
              title: 'Nothing under this pivot',
              body: 'The pivot is applied to the rows loaded, not to the whole index. Clear it to see them.',
            };
      case 'stale':
        return {
          title: 'Stale store',
          body: `${spec.label} exists but does not match the current generation. Its rows are not served as current.${read.detail ? ` ${read.detail}` : ''}`,
        };
      case 'timed_out':
        return {
          title: 'Timed out',
          body: `${spec.label} did not answer inside the admitted deadline.${read.detail ? ` ${read.detail}` : ''}`,
        };
      case 'unavailable':
        return {
          title: 'Source unavailable',
          body: `${spec.label} reported it cannot serve this run.${read.detail ? ` ${read.detail}` : ''} Nothing is substituted.`,
        };
      case 'cancelled':
        return {
          title: 'Cancelled',
          body: `The read for ${spec.label} was cancelled before it concluded.`,
        };
      case 'error':
        return {
          title: 'Failed',
          body: `${spec.label} failed.${read.detail ? ` ${read.detail}` : ''}`,
        };
      case 'offline':
        return {
          title: 'Daemon unreachable',
          body: 'The browser could not reach the daemon. This says nothing about the source itself.',
        };
      case 'unauthorized':
        return { title: 'Unauthorized', body: 'The daemon accepted no identity for this read.' };
      case 'denied':
        return { title: 'Denied', body: 'The daemon knows this identity and will not serve this scope.' };
      case 'locked':
        return { title: 'Read-only scope', body: read.detail };
      case 'unsupported_schema':
        return {
          title: 'Unsupported schema',
          body: 'The response did not decode against the generated contract.',
        };
      case 'unanswered':
        return {
          title: 'Never named',
          body: `The run concluded without naming ${spec.label}. No answer exists to show.`,
        };
      case 'unregistered':
        return {
          title: 'No production authority',
          body: `${read.detail}. Nothing is invented for this lane, and no other lane's readiness applies to it.`,
        };
      case 'indeterminate':
        return {
          title: 'Indeterminate',
          body: read.detail ?? `The transport reported “${read.domainState}”, which carries no lane reading.`,
        };
      default: {
        const exhaustive: never = read;
        return exhaustive;
      }
    }
  })();

  return (
    <div
      role="region"
      aria-label={listLabel}
      tabIndex={0}
      {...{ [LANE_LIST_ATTR]: read.lane }}
      className="td-well td-graticule flex min-h-0 flex-1 flex-col items-center justify-center gap-2 px-4 py-6 text-center"
    >
      <StateChip kind={laneStateKind(read)} />
      <p className="text-xs font-medium text-text-primary">{condition.title}</p>
      <p className="max-w-[18rem] text-2xs leading-relaxed text-text-muted">{condition.body}</p>
    </div>
  );
}

/**
 * One result row. Pointer hover and keyboard focus both PEEK — the inspector
 * shows the row without any state changing — and click or Enter SELECTS,
 * which persists as the cyan gutter. Hover therefore never substitutes for
 * focus, and the keyboard reaches every inspection the pointer can.
 */
function LaneRow({
  hit,
  terms,
  selected,
  onSelect,
  onPeek,
}: {
  hit: Hit;
  terms: readonly string[];
  selected: boolean;
  onSelect: () => void;
  onPeek: (hit: Hit | null) => void;
}) {
  const age = compactRelativeAge(hit.stamp, Date.now() / 1000);
  return (
    <button
      type="button"
      data-hit={hit.key}
      aria-pressed={selected}
      onClick={onSelect}
      onMouseEnter={() => onPeek(hit)}
      onMouseLeave={() => onPeek(null)}
      onFocus={() => onPeek(hit)}
      onBlur={() => onPeek(null)}
      style={{ height: LANE_ROW_HEIGHT }}
      className={cn(
        'relative flex w-full flex-col justify-center gap-0.5 border-b border-edge-subtle pl-3.5 pr-2.5 text-left',
        'transition-[opacity,background-color,box-shadow] duration-[var(--dur-state)] ease-[var(--ease-standard)] motion-reduce:transition-none',
        // Hover raises the face one plane and draws one restrained halo;
        // selection is the gutter, not a wash, so the two never read as the
        // same thing. Keyboard focus keeps the shell's 2px outline.
        'hover:bg-raised hover:shadow-[var(--shadow-raised)] hover:outline hover:outline-1 hover:-outline-offset-1 hover:outline-accent/40',
        'focus-visible:bg-surface-2 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent',
        selected && 'bg-surface-2',
      )}
    >
      <span
        aria-hidden
        className={cn('absolute inset-y-0 left-0 w-[3px]', selected ? 'bg-accent' : 'bg-transparent')}
      />
      <span className="flex min-w-0 items-baseline gap-2">
        <Highlight
          text={hit.title}
          terms={terms}
          className={cn(
            'min-w-0 flex-1 truncate text-xs text-text-primary',
            hit.lane === 'code' && 'font-mono',
          )}
        />
        {hit.facet ? (
          <span className="td-legend shrink-0 border border-edge-subtle px-1 py-px text-text-secondary">
            {hit.facet}
          </span>
        ) : null}
      </span>
      {hit.context ? (
        <Highlight
          text={hit.context}
          terms={terms}
          className="min-w-0 truncate font-mono text-2xs text-text-muted"
        />
      ) : hit.body ? (
        <Highlight text={hit.body} terms={terms} className="min-w-0 truncate text-2xs text-text-muted" />
      ) : (
        <span className="text-2xs text-text-muted">{hit.titleField}</span>
      )}
      <span className="flex min-w-0 items-center gap-2 text-3xs text-text-muted">
        <span className="td-value text-3xs text-text-muted" data-cell="numeric">
          #{hit.rank}
        </span>
        {hit.signal ? (
          <span className="flex items-center gap-1.5">
            <Meter
              fraction={hit.signal.max > 0 ? hit.signal.value / hit.signal.max : null}
              className="w-10"
              height="row"
              tone="bg-accent/80"
              ariaLabel={`${hit.signal.field} ${hit.signal.value}`}
            />
            <span className="tabular">{hit.signal.display}</span>
          </span>
        ) : null}
        {age ? <span className="ml-auto tabular">{age}</span> : null}
      </span>
    </button>
  );
}

/** Roving arrows over the rows of ONE lane: rows are native buttons, so
 * Enter/Space activate for free; the vertical arrows, Home, End and the Page
 * keys move focus without a Tab through every row. The horizontal arrows are
 * left to the grid, which owns lane order. */
function rovingRows(event: KeyboardEvent<HTMLDivElement>) {
  const rows = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('button[data-hit]')];
  if (rows.length === 0) return;
  const active = document.activeElement;
  const current = active instanceof HTMLButtonElement ? rows.indexOf(active) : -1;
  const last = rows.length - 1;
  const from = current < 0 ? 0 : current;
  let next: number;
  switch (event.key) {
    case 'Home':
      next = 0;
      break;
    case 'End':
      next = last;
      break;
    case 'PageDown':
      next = Math.min(from + 10, last);
      break;
    case 'PageUp':
      next = Math.max(from - 10, 0);
      break;
    case 'ArrowDown':
      next = Math.min(current + 1, last);
      break;
    case 'ArrowUp':
      next = Math.max(current - 1, 0);
      break;
    default:
      return;
  }
  event.preventDefault();
  rows[next]?.focus();
  rows[next]?.scrollIntoView({ block: 'nearest' });
}

/** The lanes grid: the four columns and the horizontal-arrow traversal
 * between them. Moving left or right lands on the same row index in the
 * neighbouring lane, or on that lane's region when it has no rows, so a
 * keyboard reader can walk across the authorities the way a pointer would. */
export function LaneGrid({ children }: { children: ReactNode }) {
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
    const active = document.activeElement;
    if (!(active instanceof HTMLElement)) return;
    const lists = [...event.currentTarget.querySelectorAll<HTMLElement>(`[${LANE_LIST_ATTR}]`)];
    const from = lists.findIndex((list) => list === active || list.contains(active));
    if (from < 0) return;
    const to = event.key === 'ArrowRight' ? from + 1 : from - 1;
    const target = lists[to];
    if (target === undefined) return;
    event.preventDefault();
    const currentRows = [...lists[from]!.querySelectorAll<HTMLButtonElement>('button[data-hit]')];
    const index = active instanceof HTMLButtonElement ? currentRows.indexOf(active) : -1;
    const targetRows = [...target.querySelectorAll<HTMLButtonElement>('button[data-hit]')];
    const row = targetRows[Math.min(Math.max(index, 0), targetRows.length - 1)];
    (row ?? target).focus();
    row?.scrollIntoView({ block: 'nearest' });
  };
  return (
    <div
      role="group"
      aria-label="Result lanes"
      onKeyDown={onKeyDown}
      className={cn(
        'td-stagger grid gap-2 p-2 [grid-template-columns:repeat(auto-fit,minmax(15rem,1fr))]',
        // One row at `lg` and above: the grid takes the aperture's height and
        // each lane scrolls its own rows. Where the lanes wrap — narrow
        // viewports, 200% zoom — every row is a fixed lane height instead of
        // a share of a height too small to hold them, so wrapped lanes stack
        // and the page scrolls rather than the rows overlapping.
        'lg:min-h-0 lg:flex-1 lg:auto-rows-fr',
        'max-lg:flex-none max-lg:auto-rows-[28rem]',
      )}
    >
      {children}
    </div>
  );
}
