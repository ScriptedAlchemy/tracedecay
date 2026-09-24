import { cn } from '../../ui/cn';
import type { TopologyMark } from './delegationTopology.ts';
import type { TopologyInteraction } from './DelegationTopology.tsx';

/**
 * The delegation field's renderers. All of them draw the same fitted model,
 * the same reconciled population, and hand the same inspect/select acts to
 * the page, so switching renderer never changes what the reading says, only
 * which dimension carries it: generation columns, descendant-scaled rings,
 * recorded time, or depth rings around an origin.
 */
export type TopologyVariant = 'generations' | 'rings' | 'timeline' | 'radial';

export const TOPOLOGY_VARIANTS: readonly {
  readonly value: TopologyVariant;
  readonly label: string;
  readonly note: string;
}[] = [
  { value: 'generations', label: 'Generations', note: 'columns by generation · discs' },
  { value: 'rings', label: 'Rings', note: 'columns by generation · hollow rings sized by sessions beneath' },
  { value: 'timeline', label: 'Timeline', note: 'recorded start and end on x · hierarchy on y' },
  { value: 'radial', label: 'Radial', note: 'generation as ring · siblings by angle' },
];

export function TopologyVariantSwitch({
  value,
  onChange,
}: {
  value: TopologyVariant;
  onChange: (next: TopologyVariant) => void;
}) {
  const active = TOPOLOGY_VARIANTS.find((variant) => variant.value === value);
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1">
      <div
        role="radiogroup"
        aria-label="Topology renderer"
        className="flex shrink-0 items-center border border-edge-subtle rounded-[var(--radius-panel)]"
      >
        {TOPOLOGY_VARIANTS.map((variant) => {
          const checked = variant.value === value;
          return (
            <button
              key={variant.value}
              type="button"
              role="radio"
              aria-checked={checked}
              title={variant.note}
              onClick={() => onChange(variant.value)}
              data-topology-variant={variant.value}
              className={cn(
                'td-hit px-2.5 text-2xs',
                checked
                  ? 'bg-surface-2 text-text-primary shadow-[inset_0_-2px_0_var(--raw-accent)]'
                  : 'text-text-muted hover:text-text-primary',
              )}
            >
              {variant.label}
            </button>
          );
        })}
      </div>
      {active ? <span className="td-legend whitespace-normal">{active.note}</span> : null}
    </div>
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
