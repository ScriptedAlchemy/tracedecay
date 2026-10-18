/**
 * The sections rail: one entry per configuration group the payload serves,
 * in origin order, each naming where the group is read from and how many of
 * its keys the current filter shows. Activating an entry scrolls the table to
 * that group; it never filters, scopes, or writes.
 */

import { cn } from '../../ui/cn';
import type { ConfigSection } from './settingsModel.ts';
import { OriginMark } from './SettingsValues.tsx';

export function SectionsRail({
  entries,
  total,
  activeId,
  onJump,
}: {
  entries: ReadonlyArray<{ section: ConfigSection; count: number }>;
  total: number;
  /** The group holding the inspected or selected row, marked as position. */
  activeId: string | null;
  onJump: (id: string) => void;
}) {
  return (
    <nav
      aria-label="Configuration groups"
      tabIndex={0}
      className="flex max-h-32 w-full shrink-0 flex-col overflow-auto border-b border-edge-subtle bg-surface-1 lg:max-h-none lg:w-44 lg:border-b-0 lg:border-r"
    >
      <div className="flex h-8 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5">
        <span className="td-title">
          {entries.length === total ? 'Sections' : `${entries.length}/${total} sections`}
        </span>
        <span aria-hidden className="td-rule" />
      </div>
      <div className="grid grid-cols-2 p-1.5 sm:grid-cols-3 lg:flex lg:flex-col">
        {entries.map(({ section, count }) => {
          const active = section.id === activeId;
          return (
            <button
              key={section.id}
              type="button"
              onClick={() => onJump(section.id)}
              aria-current={active ? 'true' : undefined}
              className={cn(
                'relative flex min-h-[var(--touch-target-min)] items-center gap-2 px-2 py-1.5 text-left text-xs text-text-secondary hover:bg-surface-2 hover:text-text-primary focus-visible:bg-surface-2',
                active && 'bg-surface-2 text-text-primary',
              )}
            >
              <span
                aria-hidden
                className={cn(
                  'absolute inset-y-1 left-0 w-[3px]',
                  active ? 'bg-accent' : 'bg-transparent',
                )}
              />
              <OriginMark origin={section.origin} />
              <span className="min-w-0 flex-1 truncate">{section.title}</span>
              <span className="td-value shrink-0 text-xs text-text-muted" data-cell="numeric">
                {count}
              </span>
            </button>
          );
        })}
      </div>
    </nav>
  );
}
