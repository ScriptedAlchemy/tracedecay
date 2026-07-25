import type { ReactNode } from 'react';
import { cn } from '../cn';
import { MetaLabel } from './Highlight.tsx';

/** A pivot dimension: one row per value, with the real count beside it and a
 * proportional rule underneath so the shape of the result set is legible
 * before you read a single number. */
export interface Facet {
  readonly id: string;
  readonly label: string;
  readonly count: number;
  readonly hint?: string;
  readonly trailing?: ReactNode;
}

export function FacetGroup({
  title,
  note,
  facets,
  active,
  onToggle,
  emptyNote,
}: {
  title: string;
  note?: string;
  facets: readonly Facet[];
  /** null selects everything. */
  active: string | null;
  onToggle: (id: string | null) => void;
  emptyNote?: string;
}) {
  const max = facets.reduce((m, f) => Math.max(m, f.count), 0);
  return (
    <section className="flex flex-col gap-1.5">
      <div className="flex items-baseline justify-between gap-2">
        <MetaLabel>{title}</MetaLabel>
        {active !== null ? (
          <button
            type="button"
            onClick={() => onToggle(null)}
            className="text-2xs text-accent underline-offset-2 hover:underline"
          >
            clear
          </button>
        ) : note ? (
          <span className="text-2xs text-text-muted">{note}</span>
        ) : null}
      </div>
      {facets.length === 0 ? (
        <p className="text-2xs text-text-muted">{emptyNote ?? 'nothing to pivot on yet'}</p>
      ) : (
        <ul className="flex flex-col">
          {facets.map((facet) => {
            const selected = active === facet.id;
            return (
              <li key={facet.id}>
                <button
                  type="button"
                  aria-pressed={selected}
                  onClick={() => onToggle(selected ? null : facet.id)}
                  className={cn(
                    'group flex w-full flex-col gap-1 rounded-[var(--radius-chip)] px-1.5 py-1 text-left',
                    'hover:bg-surface-2',
                    selected && 'bg-surface-2',
                  )}
                >
                  <span className="flex w-full items-baseline gap-2">
                    <span
                      className={cn(
                        'min-w-0 flex-1 truncate text-xs',
                        selected ? 'font-semibold text-text-primary' : 'text-text-secondary',
                      )}
                    >
                      {facet.label}
                    </span>
                    {facet.trailing}
                    <span className="tabular shrink-0 text-2xs text-text-muted">
                      {facet.count.toLocaleString()}
                    </span>
                  </span>
                  <span
                    aria-hidden
                    className="block h-px w-full overflow-hidden bg-edge-subtle"
                  >
                    <span
                      className={cn(
                        'block h-px',
                        selected ? 'bg-accent' : 'bg-edge-strong group-hover:bg-accent/60',
                      )}
                      style={{ width: `${max === 0 ? 0 : (facet.count / max) * 100}%` }}
                    />
                  </span>
                  {facet.hint ? (
                    <span className="text-2xs text-text-muted">{facet.hint}</span>
                  ) : null}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

/**
 * A measured quantity the daemon actually returned (graph degree, fact trust),
 * drawn as a proportional rule. It is never a relevance score: the label always
 * names the field it came from.
 */
export function Meter({
  value,
  max,
  label,
  className,
}: {
  value: number;
  max: number;
  label: string;
  className?: string;
}) {
  const fraction = max <= 0 ? 0 : Math.min(Math.max(value / max, 0), 1);
  return (
    <span
      className={cn('inline-flex items-center gap-1.5', className)}
      title={label}
      role="img"
      aria-label={label}
    >
      <span aria-hidden className="block h-1 w-10 overflow-hidden rounded-full bg-surface-3">
        <span
          className="block h-1 rounded-full bg-accent/80"
          style={{ width: `${fraction * 100}%` }}
        />
      </span>
    </span>
  );
}

/** Inline dot-separated metadata, aligned to a shared baseline. */
export function MetaRow({ children }: { children: ReactNode }) {
  return (
    <span className="flex min-w-0 shrink-0 items-center gap-2 text-2xs text-text-muted">
      {children}
    </span>
  );
}
