import { cn } from '../../ui/cn.ts';
import type { StructureLens } from './structureLens.ts';

const LENSES: readonly {
  lens: StructureLens;
  label: string;
  scale: string;
}[] = [
  { lens: 'cortex', label: 'CORTEX', scale: 'repository' },
  { lens: 'trace', label: 'TRACE', scale: 'symbol' },
  { lens: 'core', label: 'CORE', scale: 'file sample' },
];

/** One aggregation ruler, not three pages. The focus identity survives every
 * move; unavailable positions are disabled instead of populated with a
 * guessed symbol or source file. */
export function StructureLensRuler({
  lens,
  focusAvailable,
  coreAvailable,
  onChange,
}: {
  lens: StructureLens;
  focusAvailable: boolean;
  coreAvailable: boolean;
  onChange: (lens: StructureLens) => void;
}) {
  return (
    <nav
      aria-label="Structure lens"
      className="flex min-h-[var(--touch-target-min)] shrink-0 items-stretch border-b border-edge-subtle bg-surface-1"
    >
      <span className="td-legend flex items-center border-r border-edge-subtle px-3">
        lens
      </span>
      <ol className="flex min-w-0 flex-1 items-stretch">
        {LENSES.map((entry, index) => {
          const disabled =
            entry.lens === 'trace'
              ? !focusAvailable
              : entry.lens === 'core'
                ? !coreAvailable
                : false;
          return (
            <li key={entry.lens} className="flex min-w-0 flex-1 items-stretch">
              <button
                type="button"
                aria-current={lens === entry.lens ? 'step' : undefined}
                disabled={disabled}
                onClick={() => onChange(entry.lens)}
                className={cn(
                  'relative flex min-h-[var(--touch-target-min)] min-w-0 flex-1 flex-col justify-center px-3 text-left',
                  'border-r border-edge-subtle hover:bg-surface-2 disabled:cursor-not-allowed disabled:opacity-40',
                  lens === entry.lens && 'bg-surface-2',
                )}
              >
                <span className="td-value text-2xs text-text-primary">{entry.label}</span>
                <span className="td-legend truncate normal-case tracking-normal">
                  {entry.scale}
                </span>
                {lens === entry.lens ? (
                  <span
                    aria-hidden
                    className="absolute inset-x-2 bottom-0 h-0.5 bg-accent-primary"
                  />
                ) : null}
              </button>
              {index < LENSES.length - 1 ? (
                <span
                  aria-hidden
                  className="pointer-events-none -ml-1 flex w-2 items-center text-text-muted"
                >
                  ›
                </span>
              ) : null}
            </li>
          );
        })}
      </ol>
      <span className="td-legend flex items-center px-3 normal-case tracking-normal text-text-muted max-sm:hidden">
        aggregation decreases →
      </span>
    </nav>
  );
}
