import { cn } from '../../ui/cn.ts';
import {
  CODE_VIEW_DEFINITIONS,
  CODE_VIEWS,
  codeViewNeedsFocus,
  type CodeView,
} from './codeView.ts';

export const CODE_VIEW_PANEL_ID = 'code-view-panel';

export function codeViewControlId(view: CodeView): string {
  return `code-view-${view}`;
}

export function codeViewNote(view: CodeView): string {
  return CODE_VIEW_DEFINITIONS[view].note;
}

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
      className="flex min-w-0 flex-wrap items-center gap-1 border-b border-edge-subtle bg-surface-1 p-1"
      data-code-view={active}
    >
      <ol className="flex min-w-0 flex-wrap items-center gap-1">
        {CODE_VIEWS.map((view) => {
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
                onClick={() => onSelect(view)}
                className={cn(
                  'flex min-h-[44px] items-center gap-2 border px-3 text-2xs',
                  'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-40',
                  selected
                    ? 'border-edge-strong bg-surface-3 text-text-primary'
                    : 'border-transparent text-text-secondary hover:bg-surface-2',
                )}
              >
                <span
                  aria-hidden
                  className={cn(
                    'h-3 w-px shrink-0',
                    selected ? 'bg-accent' : 'bg-edge-strong',
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
