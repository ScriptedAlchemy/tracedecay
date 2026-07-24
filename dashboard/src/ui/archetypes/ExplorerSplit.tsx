import { useRef, type ReactNode } from 'react';
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
  className?: string;
}) {
  const resultsRef = useRef<HTMLElement | null>(null);
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
      <div className="flex min-h-0 flex-1">
        {filters ? (
          <aside
            aria-label="Filters"
            className="flex w-56 shrink-0 flex-col border-r border-edge-subtle bg-surface-1 max-lg:hidden"
          >
            <BayLegend>Query</BayLegend>
            <div className="min-h-0 flex-1 overflow-auto p-2.5">{filters}</div>
          </aside>
        ) : null}
        <section
          ref={resultsRef}
          aria-label="Results"
          className="flex min-w-0 flex-1 flex-col overflow-hidden"
          onKeyDown={onResultsKeyDown}
        >
          <div className="min-h-0 flex-1 overflow-auto">{list}</div>
        </section>
        {inspector ? (
          <aside
            aria-label="Inspector"
            className="w-[22rem] shrink-0 overflow-auto border-l border-edge-subtle bg-surface-1 max-xl:w-72 max-md:hidden"
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
    <div className="flex h-7 shrink-0 items-center gap-2 border-b border-edge-subtle px-2.5">
      <span className="td-legend">{children}</span>
      <span aria-hidden className="td-rule" />
    </div>
  );
}

/** 32px data row: hairline-ruled, monospaced, with a selection lamp in the
 * gutter so a picked row is legible without a fill wash. */
export function DataRow({
  selected,
  onSelect,
  children,
  className,
}: {
  selected?: boolean;
  onSelect?: () => void;
  children: ReactNode;
  className?: string;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected ?? false}
      className={cn(
        'relative flex h-8 w-full items-center gap-3 border-b border-edge-subtle pl-3 pr-3 text-left text-xs',
        'hover:bg-surface-1 focus-visible:bg-surface-1',
        selected && 'bg-surface-2',
        className,
      )}
    >
      <span
        aria-hidden
        className={cn(
          'absolute inset-y-0 left-0 w-[2px]',
          selected ? 'bg-accent' : 'bg-transparent',
        )}
      />
      {children}
    </button>
  );
}

export function InspectorPanel({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose?: () => void;
  children: ReactNode;
}) {
  return (
    <div className="flex h-full flex-col">
      <header className="flex h-7 shrink-0 items-center gap-2 border-b border-edge-subtle px-2.5">
        <h2 className="td-legend truncate text-text-secondary">{title}</h2>
        <span aria-hidden className="td-rule" />
        {onClose ? (
          <button
            type="button"
            onClick={onClose}
            aria-label="Close inspector"
            className="shrink-0 px-1 text-text-muted hover:text-text-primary"
          >
            ×
          </button>
        ) : null}
      </header>
      <div className="min-h-0 flex-1 overflow-auto p-2.5">{children}</div>
    </div>
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

/** Generic key/value renderer for legacy payload inspection: honest raw data
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
          // minmax(...) lets the label column give way under pressure (deep
          // nesting, a 320px viewport) instead of reserving a hard 8rem no
          // matter what — a fixed track never shrinks, so a narrow container
          // forced the value column to negative space and every value
          // wrapped one character per line.
          className="grid grid-cols-[minmax(5rem,9rem)_1fr] gap-2 border-b border-edge-subtle/60 py-1 text-2xs last:border-b-0"
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
