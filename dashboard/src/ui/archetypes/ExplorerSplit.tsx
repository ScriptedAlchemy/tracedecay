import type { ReactNode } from 'react';
import { cn } from '../cn';

/** Archetype 2 (plan 11a): left filter column, center result list, right
 * inspector. Regions are slots; workspaces own only read-model wiring. */
export function ExplorerSplit({
  filters,
  list,
  inspector,
  className,
}: {
  filters?: ReactNode;
  list: ReactNode;
  inspector?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('flex h-full min-h-0', className)}>
      {filters ? (
        <aside
          aria-label="Filters"
          className="w-56 shrink-0 overflow-auto border-r border-edge-subtle bg-surface-1 p-3 max-lg:hidden"
        >
          {filters}
        </aside>
      ) : null}
      <section aria-label="Results" className="min-w-0 flex-1 overflow-auto">
        {list}
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
  );
}

/** 36px data row (plan 11a rhythm) with leading state/selection affordance. */
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
        'flex h-9 w-full items-center gap-3 border-b border-edge-subtle px-3 text-left text-xs',
        'hover:bg-surface-1 focus-visible:bg-surface-1',
        selected && 'bg-surface-2',
        className,
      )}
    >
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
      <header className="flex h-10 shrink-0 items-center justify-between border-b border-edge-subtle px-3">
        <h2 className="truncate text-xs font-semibold tracking-tight">{title}</h2>
        {onClose ? (
          <button
            type="button"
            onClick={onClose}
            aria-label="Close inspector"
            className="rounded px-1.5 text-text-muted hover:text-text-primary"
          >
            ×
          </button>
        ) : null}
      </header>
      <div className="min-h-0 flex-1 overflow-auto p-3">{children}</div>
    </div>
  );
}

/** Generic key/value renderer for legacy payload inspection: honest raw data
 * presentation until a typed view lands per family. */
export function KeyValueTree({ value, depth = 0 }: { value: unknown; depth?: number }) {
  if (value === null || value === undefined) {
    return <span className="text-text-muted">—</span>;
  }
  if (typeof value !== 'object') {
    return <span className="tabular break-all text-text-secondary">{String(value)}</span>;
  }
  const entries = Array.isArray(value)
    ? value.map((v, i) => [String(i), v] as const)
    : Object.entries(value as Record<string, unknown>);
  if (entries.length === 0) return <span className="text-text-muted">empty</span>;
  return (
    <dl className={cn('flex flex-col gap-1', depth > 0 && 'border-l border-edge-subtle pl-2')}>
      {entries.slice(0, 60).map(([k, v]) => (
        <div key={k} className="grid grid-cols-[9rem_1fr] gap-2 text-2xs">
          <dt className="truncate text-text-muted" title={k}>
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
