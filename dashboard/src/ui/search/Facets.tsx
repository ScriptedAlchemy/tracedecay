import type { ReactNode } from 'react';
import { cn } from '../cn';
import { Meter } from '../instrument.tsx';
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
                    // A facet is a full-width list row, so the row is the
                    // target and it carries the minimum directly.
                    'group flex min-h-[var(--touch-target-min)] w-full flex-col justify-center gap-1 rounded-[var(--radius-chip)] px-1.5 py-1 text-left',
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
                  <Meter
                    // With no facet carrying a count there is no denominator to
                    // draw against, so the track stays empty rather than
                    // showing a zero-width fill as though zero were measured.
                    fraction={max === 0 ? null : facet.count / max}
                    height="hairline"
                    className="w-full bg-edge-subtle"
                    tone={selected ? 'bg-accent' : 'bg-edge-strong group-hover:bg-accent/60'}
                  />
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
