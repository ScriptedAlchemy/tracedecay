import { useId, useRef, useState, type ReactNode } from 'react';
import { ChevronDown } from 'lucide-react';
import { cn } from '../cn';
import { WorkspaceHeader } from '../instrument.tsx';

/** Archetype 2 (plan 11a): left filter column, center result list, right
 * inspector. Regions are slots; workspaces own only read-model wiring.
 *
 * The three columns are divided by hairlines and each carries an engraved
 * legend so the split reads as three instrument bays rather than three
 * unlabeled scroll areas. */
export function ExplorerSplit({
  path,
  title,
  note,
  filters,
  list,
  inspector,
  header,
  stackOnNarrow = false,
  inspectorWidth = 'standard',
  className,
}: {
  /** Workspace path — supplies the channel number in the header. Omit to
   * render the split without a header (embedded use). */
  path?: string;
  title?: string;
  note?: ReactNode;
  filters?: ReactNode;
  list: ReactNode;
  inspector?: ReactNode;
  /** Full-width workspace controls rendered above the split columns. */
  header?: ReactNode;
  /** Keep filters and inspector reachable by stacking them below `lg`. */
  stackOnNarrow?: boolean;
  inspectorWidth?: 'standard' | 'wide';
  className?: string;
}) {
  const resultsRef = useRef<HTMLElement | null>(null);
  // Below `lg` the filter rail is display:none, which used to take the query
  // input — the only way to search — with it. The archetype owns the fix: the
  // same `filters` node renders a second time as a collapsible strip above the
  // results, shown only below `lg` (the rail and the strip are never both
  // visible). Filter state lives in the workspace, so both renders stay in
  // sync; collapsed by default so narrow viewports keep their vertical budget.
  const [mobileFiltersOpen, setMobileFiltersOpen] = useState(false);
  const mobileFiltersId = useId();
  // Roving arrows over the result rows (plan 11 keyboard model): rows are
  // native buttons, so Enter/Space activate for free; arrows, Home, End and
  // Page keys move focus without forcing a Tab-through of every row.
  const onResultsKeyDown = (event: React.KeyboardEvent) => {
    const keys = ['ArrowDown', 'ArrowUp', 'Home', 'End', 'PageDown', 'PageUp'];
    if (!keys.includes(event.key)) return;
    const container = resultsRef.current;
    if (!container) return;
    const rows = [...container.querySelectorAll<HTMLButtonElement>('button')];
    if (rows.length === 0) return;
    const current = rows.indexOf(document.activeElement as HTMLButtonElement);
    const page = 10;
    const next =
      event.key === 'Home'
        ? 0
        : event.key === 'End'
          ? rows.length - 1
          : event.key === 'PageDown'
            ? Math.min((current < 0 ? 0 : current) + page, rows.length - 1)
            : event.key === 'PageUp'
              ? Math.max((current < 0 ? 0 : current) - page, 0)
              : event.key === 'ArrowDown'
                ? Math.min(current + 1, rows.length - 1)
                : Math.max(current - 1, 0);
    event.preventDefault();
    rows[next]?.focus();
    rows[next]?.scrollIntoView({ block: 'nearest' });
  };
  return (
    <div className={cn('flex h-full min-h-0 flex-col', className)}>
      {path && title ? <WorkspaceHeader path={path} title={title} note={note} /> : null}
      {header}
      {filters && !stackOnNarrow ? (
        <div className="shrink-0 border-b border-edge-subtle bg-surface-1 lg:hidden">
          <button
            type="button"
            aria-expanded={mobileFiltersOpen}
            // The reference exists only while the panel does: a collapsed
            // disclosure has no panel in the tree, and an `aria-controls`
            // naming an absent element is the same critical
            // `aria-valid-attr-value` failure the work projection panel had.
            // (Mounting the panel `hidden` instead would render `filters` a
            // second time in every state; this strip already re-renders the
            // rail's node, so the collapsed copy would make it three.)
            aria-controls={mobileFiltersOpen ? mobileFiltersId : undefined}
            onClick={() => setMobileFiltersOpen((open) => !open)}
            // The only way to reach filters below `lg`, at 28px tall.
            className="flex min-h-[var(--touch-target-min)] w-full items-center gap-2.5 px-2.5 text-left"
          >
            <span className="td-title">Query</span>
            <span aria-hidden className="td-rule" />
            <ChevronDown
              aria-hidden
              size={13}
              className={cn(
                'shrink-0 text-text-muted',
                mobileFiltersOpen && 'rotate-180',
              )}
            />
          </button>
          {mobileFiltersOpen ? (
            <div
              id={mobileFiltersId}
              role="region"
              aria-label="Filter controls"
              // Scrollable regions need keyboard operation (WCAG 2.1.1). A
              // filter column whose current content is all read-out — no facet
              // buttons because nothing loaded — is scrollable with nothing
              // inside to tab to, so the panel itself takes the tab stop.
              tabIndex={0}
              className="max-h-[45vh] overflow-auto border-t border-edge-subtle p-2.5"
            >
              {filters}
            </div>
          ) : null}
        </div>
      ) : null}
      <div className={cn('flex min-h-0 flex-1', stackOnNarrow && 'max-lg:flex-col')}>
        {filters ? (
          <aside
            aria-label="Filters"
            className={cn(
              'flex w-56 shrink-0 flex-col border-r border-edge-subtle bg-surface-1',
              stackOnNarrow
                ? 'max-lg:max-h-64 max-lg:w-full max-lg:border-b max-lg:border-r-0'
                : 'max-lg:hidden',
            )}
          >
            <BayLegend>Query</BayLegend>
            {/* The aside's name lives on the landmark; the element a reader
              * actually scrolls is this inner div, so it carries its own name
              * (axe: scrollable-region-focusable wants the operable node
              * labelled, not an ancestor). */}
            <div
              role="region"
              aria-label="Filter controls"
              tabIndex={0}
              className="min-h-0 flex-1 overflow-auto p-2.5"
            >
              {filters}
            </div>
          </aside>
        ) : null}
        <section
          ref={resultsRef}
          aria-label="Results"
          // The results pane is the one member of this split that is allowed to
          // shrink — its only child is a scroll container, whose automatic
          // minimum size is zero — so it absorbed every deficit the layout had.
          // Stacked below `lg` the filter rail took its 224px first and left
          // the results at `height: 0`; the rows and the scrollbar that would
          // have reached them both disappeared while the caption above went on
          // reporting "7 rows across 3 memories". The floor refuses that
          // division: when the viewport cannot pay for it, the split overflows
          // and `main#td-main` scrolls, which is a reader scrolling a page
          // rather than a reader losing the answer.
          className="flex min-h-[var(--pane-min-height)] min-w-0 flex-1 flex-col overflow-hidden"
          onKeyDown={onResultsKeyDown}
        >
          {/* Named, because Plan 11 licenses internal scrolling for LABELLED
            * regions only, and this is the element that actually scrolls — the
            * section around it is `overflow-hidden`, so its own name never
            * reaches the scroll container a reader operates. */}
          <div
            role="region"
            aria-label="Result rows"
            // Empty and refused readings render no focusable rows, so the
            // scroll container itself must stay keyboard-operable.
            tabIndex={0}
            className="min-h-0 flex-1 overflow-auto"
          >
            {list}
          </div>
        </section>
        {inspector ? (
          <aside
            aria-label="Inspector"
            className={cn(
              'shrink-0 overflow-auto border-l border-edge-subtle bg-surface-1',
              inspectorWidth === 'wide'
                ? 'w-[30rem] max-xl:w-[23rem]'
                : 'w-[22rem] max-xl:w-72',
              stackOnNarrow
                ? 'max-lg:h-72 max-lg:w-full max-lg:border-l-0 max-lg:border-t'
                : 'max-md:hidden',
            )}
          >
            {inspector}
          </aside>
        ) : null}
      </div>
    </div>
  );
}

/** The engraved legend that names an instrument bay, sitting on its own
 * hairline at the top of the column. */
export function BayLegend({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-8 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5">
      <span className="td-title">{children}</span>
      <span aria-hidden className="td-rule" />
    </div>
  );
}

/** The data row: hairline-ruled, monospaced, with a selection lamp in the
 * gutter so a picked row is legible without a fill wash.
 *
 * Height comes from `--row-height-data` rather than a utility class, because
 * `VirtualList` derives its windowing estimate from the same token; if the two
 * disagree, every row of a windowed list is mispositioned.
 *
 * The row is a grid rather than a flex line: a fixed leading gutter means the
 * magnitude rails in a column line up exactly, which is the entire point of
 * drawing them. */
export function DataRow({
  selected,
  onSelect,
  children,
  className,
  height,
  align = 'center',
  railClassName,
}: {
  selected?: boolean;
  onSelect?: () => void;
  children: ReactNode;
  className?: string;
  /**
   * Fixed row height in pixels, for lists whose rows carry more than one line.
   * Still FIXED, never intrinsic: `VirtualList` positions windowed rows from a
   * single estimate, so a row that measures itself would offset every row
   * below it. Defaults to the `--row-height-data` token every other list uses.
   */
  height?: number;
  /** Multi-line rows read from the top; single-line rows stay centred. */
  align?: 'center' | 'start';
  /** Optional categorical rail. Selection remains visible through opacity. */
  railClassName?: string;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected ?? false}
      style={{ height: height != null ? `${height}px` : 'var(--row-height-data)' }}
      className={cn(
        'relative flex w-full gap-3 border-b border-edge-subtle pl-3 pr-3 text-left text-xs',
        align === 'start' ? 'items-start pt-2' : 'items-center',
        'hover:bg-surface-1 focus-visible:bg-surface-1',
        // Lists in this archetype pin a `ListCaption` at `top-0`, so a row
        // brought into view by the roving arrow keys landed underneath it —
        // focused but hidden, and unclickable at the same coordinates. The
        // scroll margin parks the row below the caption instead.
        'scroll-mt-9',
        selected && 'bg-surface-2',
        className,
      )}
    >
      <span
        aria-hidden
        className={cn(
          'absolute inset-y-0 left-0',
          railClassName
            ? cn('w-[3px]', railClassName, selected ? 'opacity-100' : 'opacity-45')
            : cn('w-[2px]', selected ? 'bg-accent' : 'bg-transparent'),
        )}
      />
      {children}
    </button>
  );
}

/** Fixed height shared by two-line result rows and their virtualizer. */
export const RESULT_ROW_HEIGHT = 56;

export function ListCaption({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <p
      className={cn(
        'sticky top-0 z-10 flex items-center gap-2 border-b border-edge-subtle bg-surface-0/95 px-4 py-2 text-2xs text-text-muted backdrop-blur',
        className,
      )}
    >
      {children}
    </p>
  );
}

export function InspectorPanel({
  title,
  eyebrow,
  onClose,
  children,
}: {
  title: string;
  eyebrow?: ReactNode;
  onClose?: () => void;
  children: ReactNode;
}) {
  return (
    <div className="flex h-full flex-col">
      <header className="flex min-h-10 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5 py-2">
        <span className="flex min-w-0 flex-col gap-0.5">
          {eyebrow ? (
            <span className="flex items-center gap-1.5 text-2xs uppercase tracking-[0.08em] text-text-muted">
              {eyebrow}
            </span>
          ) : null}
          <h2 className="td-title truncate">{title}</h2>
        </span>
        <span aria-hidden className="td-rule" />
        {onClose ? (
          <button
            type="button"
            onClick={onClose}
            aria-label="Close inspector"
            // 15x21 was the glyph's own box, which is the smallest control in
            // the product. The × stays exactly the size it was — the hit area
            // grows around it instead, and the negative margin lets it use the
            // header's padding so the bar does not gain 16px to hold it.
            className="-my-2 flex size-[var(--touch-target-min)] shrink-0 items-center justify-center text-text-muted hover:text-text-primary"
          >
            ×
          </button>
        ) : null}
      </header>
      {/* The panel body is the scroll container inside the Inspector aside;
        * inspector content is frequently pure read-out with nothing focusable,
        * so it takes the tab stop and carries the panel's own title. */}
      <div
        role="region"
        aria-label={title}
        tabIndex={0}
        className="min-h-0 flex-1 overflow-auto p-2.5"
      >
        {children}
      </div>
    </div>
  );
}

export function RawFields({
  value,
  label = 'Every field the daemon returned',
}: {
  value: unknown;
  label?: string;
}) {
  return (
    <details className="mt-4 border-t border-edge-subtle pt-3">
      {/* Grown to the touch minimum by min-height and block alignment rather
        * than by `flex`: a `<summary>` only draws its disclosure marker while
        * it is `display: list-item`, and that triangle is the entire signal
        * that this row opens. Losing it to satisfy a size check would trade a
        * real affordance for a number. */}
      <summary className="min-h-[var(--touch-target-min)] cursor-pointer content-center text-2xs uppercase tracking-[0.08em] text-text-muted hover:text-text-primary">
        {label}
      </summary>
      <div className="mt-2">
        <KeyValueTree value={value} />
      </div>
    </details>
  );
}

/** Filesystem paths and URLs carry no spaces, so the browser's only line-break
 * fallback (`overflow-wrap: break-word`) had nowhere to break but mid-word —
 * every character landed on its own line in a narrow column (worst at
 * 320px). A `<wbr>` after each path separator gives it a real break point
 * instead, so long paths wrap at segment boundaries like `.tracedecay/` \
 * `config.toml` rather than one letter per line. Plain values are unaffected
 * — this only ever inserts, never rewrites, the text. */
function withPathBreaks(text: string): ReactNode {
  if (!text.includes('/')) return text;
  const segments = text.split('/');
  const nodes: ReactNode[] = [];
  segments.forEach((segment, i) => {
    if (i > 0) {
      nodes.push('/');
      nodes.push(<wbr key={`wbr-${i}`} />);
    }
    nodes.push(segment);
  });
  return nodes;
}

/** Generic key/value renderer for uncontracted payload inspection: honest raw data
 * presentation until a typed view lands per family. */
export function KeyValueTree({ value, depth = 0 }: { value: unknown; depth?: number }) {
  if (value === null || value === undefined) {
    return <span className="text-text-muted">—</span>;
  }
  if (typeof value !== 'object') {
    const text = String(value);
    return (
      <span className="td-value break-words text-2xs text-text-secondary">
        {withPathBreaks(text)}
      </span>
    );
  }
  const isArray = Array.isArray(value);
  // A flat array of primitives (glob lists, tags, provider names — the common
  // case for config-shaped payloads) reads far better as a wrapped chip row
  // than as N index-labelled dt/dd pairs: no meaningless "0", "1", "2" legends
  // eating the label column, and no extra nesting depth for the width
  // collapse below to compound against.
  if (isArray && value.every((v) => v === null || typeof v !== 'object')) {
    if (value.length === 0) return <span className="text-text-muted">empty</span>;
    return (
      <div className="flex flex-wrap gap-1">
        {value.slice(0, 60).map((v, i) => (
          <span
            key={i}
            className="td-value break-words rounded-[var(--radius-chip)] border border-edge-subtle bg-surface-2 px-1.5 py-0.5 text-2xs text-text-secondary"
          >
            {v === null || v === undefined ? '—' : withPathBreaks(String(v))}
          </span>
        ))}
        {value.length > 60 ? (
          <span className="text-2xs text-text-muted">… {value.length - 60} more</span>
        ) : null}
      </div>
    );
  }
  const entries = isArray
    ? value.map((v, i) => [String(i), v] as const)
    : Object.entries(value as Record<string, unknown>);
  if (entries.length === 0) return <span className="text-text-muted">empty</span>;
  return (
    <dl className={cn('flex flex-col', depth > 0 && 'border-l border-edge-subtle pl-2')}>
      {entries.slice(0, 60).map(([k, v]) => (
        <div
          key={k}
          // Only the OUTERMOST level reserves a label column; every level
          // below stacks label above value. Side-by-side columns compound —
          // up to 9rem reserved per nesting level — and CSS Grid sizes the
          // label's minmax before the value's `1fr`, so a few levels deep the
          // value track measures 0px and every value wraps one character per
          // line. Capping the reservation at one track holds however deep the
          // payload nests and however narrow the container is.
          className={cn(
            'grid gap-x-2 gap-y-0.5 border-b border-edge-subtle/60 py-1 text-2xs last:border-b-0',
            depth === 0
              ? 'grid-cols-1 sm:grid-cols-[minmax(5rem,9rem)_1fr] sm:gap-y-0'
              : 'grid-cols-1',
          )}
        >
          <dt className="td-legend truncate pt-px" title={k}>
            {k}
          </dt>
          <dd className="min-w-0">
            <KeyValueTree value={v} depth={depth + 1} />
          </dd>
        </div>
      ))}
      {entries.length > 60 ? (
        <span className="text-2xs text-text-muted">… {entries.length - 60} more</span>
      ) : null}
    </dl>
  );
}
