import { useCallback } from 'react';
import { useSearchParams } from 'react-router';
import { cn } from '../../ui/cn';
import type { TopologyMark } from './delegationTopology.ts';
import type { TopologyInteraction } from './DelegationTopology.tsx';

/**
 * The Agents views. Both read the same fitted topology and hand the page the
 * same inspect/select acts; they differ in which dimension carries the
 * reading. Topology lays generations across and sizes rings by sessions
 * beneath; Timeline lays the store's recorded start and end across and keeps
 * the hierarchy down. The view lives in `?view` the way Code's lens and
 * Work's camera do, replaced rather than pushed.
 */
export type AgentsView = 'topology' | 'timeline';

export const AGENTS_VIEW_PARAM = 'view';

const AGENTS_VIEWS: readonly { readonly value: AgentsView; readonly label: string; readonly note: string }[] = [
  { value: 'topology', label: 'Topology', note: 'generations across · rings sized by sessions beneath' },
  { value: 'timeline', label: 'Timeline', note: 'recorded start → end across · hierarchy down' },
];

export function agentsViewNote(view: AgentsView): string {
  return AGENTS_VIEWS.find((candidate) => candidate.value === view)!.note;
}

export function useAgentsView(): [AgentsView, (next: AgentsView) => void] {
  const [params, setParams] = useSearchParams();
  const active: AgentsView = params.get(AGENTS_VIEW_PARAM) === 'timeline' ? 'timeline' : 'topology';
  const select = useCallback(
    (next: AgentsView) => {
      const updated = new URLSearchParams(params);
      if (next === 'topology') updated.delete(AGENTS_VIEW_PARAM);
      else updated.set(AGENTS_VIEW_PARAM, next);
      setParams(updated, { replace: true });
    },
    [params, setParams],
  );
  return [active, select];
}

/** The view tabs, in the workspace's control bar under the header: engraved,
 * the active one framed in signal cyan with a position bar. */
export function AgentsViewSwitcher({
  active,
  onSelect,
}: {
  active: AgentsView;
  onSelect: (view: AgentsView) => void;
}) {
  return (
    <nav aria-label="Agents view" className="flex min-w-0 flex-wrap items-center gap-1 p-1" data-agents-view={active}>
      <ol className="flex min-w-0 flex-wrap items-center gap-1">
        {AGENTS_VIEWS.map((view) => {
          const selected = view.value === active;
          return (
            <li key={view.value}>
              <button
                type="button"
                aria-current={selected ? 'page' : undefined}
                title={view.note}
                onClick={() => onSelect(view.value)}
                data-agents-view-option={view.value}
                className={cn(
                  'relative flex min-h-[44px] min-w-24 items-center justify-center border px-3 text-2xs uppercase tracking-[0.14em]',
                  'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
                  selected
                    ? 'border-accent/70 bg-surface-2 text-text-primary shadow-[inset_0_1px_0_var(--raw-membrane-lift)]'
                    : 'border-edge-subtle text-text-secondary hover:border-edge-strong hover:bg-surface-2',
                )}
              >
                <span aria-hidden className={cn('absolute inset-x-2 bottom-0 h-px', selected ? 'bg-accent' : 'bg-transparent')} />
                {view.label}
              </button>
            </li>
          );
        })}
      </ol>
    </nav>
  );
}

/** One accessible name for a mark, whichever renderer draws it. */
export function markAccessibleName(mark: TopologyMark, selected: boolean): string {
  if (mark.kind === 'bundle') {
    const title = mark.basis === 'remainder' ? mark.label : `${mark.sessions} × ${mark.label}`;
    return `${title}: ${mark.sessions} sessions in generation ${mark.generation}${mark.descendants > 0 ? `, ${mark.descendants} beneath them` : ''}. Open this bundle.`;
  }
  const linkWord =
    mark.node.link === 'missing_parent'
      ? ', parent not in this reading'
      : mark.node.link === 'cycle'
        ? ', on a parent cycle'
        : '';
  return `${mark.label}, ${mark.node.provider} session ${mark.node.session_id}, generation ${mark.generation}${linkWord}. ${selected ? 'Selected.' : 'Select to read its token frontier.'}`;
}

/** The pointer and keyboard wiring every renderer's mark control shares:
 * hover and focus inspect; click selects a session and opens a bundle. */
export function markHandlers(mark: TopologyMark, interaction: TopologyInteraction) {
  const inspect = () => interaction.onInspect(mark.id);
  return {
    onMouseEnter: inspect,
    onMouseLeave: () => interaction.onInspect(null),
    onFocus: inspect,
    onClick: () =>
      mark.kind === 'bundle' ? interaction.onToggleExpanded(mark.id) : interaction.onSelect(mark.id),
    'aria-label': markAccessibleName(mark, mark.id === interaction.selectedId),
    'data-topology-control': mark.kind,
    'data-topology-id': mark.id,
    ...(mark.kind === 'bundle'
      ? { 'aria-expanded': false }
      : { 'aria-pressed': mark.id === interaction.selectedId, 'data-topology-link': mark.node.link }),
  } as const;
}
