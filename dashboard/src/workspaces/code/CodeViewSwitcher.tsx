import { cn } from '../../ui/cn.ts';
import {
  CODE_VIEW_DEFINITIONS,
  codeViewNeedsFocus,
  codeViewsOffered,
  type CodeView,
} from './codeView.ts';

export const CODE_VIEW_PANEL_ID = 'code-view-panel';

export function codeViewControlId(view: CodeView): string {
  return `code-view-${view}`;
}

export function codeViewNote(view: CodeView): string {
  return CODE_VIEW_DEFINITIONS[view].note;
}

/**
 * The lens tabs: engraved, uppercase, the active one framed in the signal
 * hue with a cyan position bar. Sits in the workspace's control bar beside the
 * symbol search, so it carries no border of its own.
 */
export function CodeViewSwitcher({
  active,
  focusAvailable,
  onSelect,
}: {
  active: CodeView;
  /** A symbol occurrence is selected and resolved: the gate for every view
   * that reads one symbol (Trace, Shared Code). */
  focusAvailable: boolean;
  onSelect: (view: CodeView) => void;
}) {
  return (
    <nav
      aria-label="Code view"
      className="flex min-w-0 flex-wrap items-center gap-1 p-1"
      data-code-view={active}
    >
      <ol className="flex min-w-0 flex-wrap items-center gap-1">
        {codeViewsOffered(active).map((view) => {
          const definition = CODE_VIEW_DEFINITIONS[view];
          const selected = view === active;
          const disabled =
            definition.status === 'pending' || (codeViewNeedsFocus(view) && !focusAvailable);
          return (
            <li key={view}>
              <button
                id={codeViewControlId(view)}
                type="button"
                aria-current={selected ? 'page' : undefined}
                aria-controls={CODE_VIEW_PANEL_ID}
                disabled={disabled}
                title={
                  definition.status === 'pending'
                    ? `${definition.label} is not mounted: ${definition.note}`
                    : disabled
                      ? `${definition.label} needs a pinned symbol`
                      : definition.note
                }
                onClick={() => onSelect(view)}
                className={cn(
                  'relative flex min-h-[44px] min-w-24 items-center justify-center gap-2 border px-3 text-2xs uppercase tracking-[0.14em]',
                  'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-40',
                  selected
                    ? 'border-accent/70 bg-surface-2 text-text-primary shadow-[inset_0_1px_0_var(--raw-membrane-lift)]'
                    : 'border-edge-subtle text-text-secondary hover:border-edge-strong hover:bg-surface-2',
                )}
              >
                <span
                  aria-hidden
                  className={cn(
                    'absolute inset-x-2 bottom-0 h-px',
                    selected ? 'bg-accent' : 'bg-transparent',
                  )}
                />
                {definition.label}
              </button>
            </li>
          );
        })}
      </ol>
    </nav>
  );
}
