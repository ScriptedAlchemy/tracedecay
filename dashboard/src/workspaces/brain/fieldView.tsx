import { useSearchParams } from 'react-router';
import { cn } from '../../ui/cn.ts';

/** Keep the shipped field links: sigma names the luminous body view, now
 * rendered on the same Canvas2D runtime as the points view. */
export type BrainFieldView = 'neural' | 'sigma' | 'points' | 'atlas';

const VIEWS: ReadonlyArray<{ value: BrainFieldView; label: string }> = [
  { value: 'neural', label: 'Neural' },
  { value: 'sigma', label: 'Luminous' },
  { value: 'points', label: 'Points' },
  { value: 'atlas', label: 'Atlas' },
];

export function useBrainFieldView(): [BrainFieldView, (view: BrainFieldView) => void] {
  const [params, setParams] = useSearchParams();
  const value = params.get('field');
  const active = value === 'sigma' || value === 'points' || value === 'atlas' ? value : 'neural';
  return [active, (view) => {
    const next = new URLSearchParams(params);
    next.set('field', view);
    setParams(next, { replace: true, preventScrollReset: true });
  }];
}

export function BrainFieldViewSwitcher({ active, onSelect }: {
  active: BrainFieldView;
  onSelect: (view: BrainFieldView) => void;
}) {
  return (
    <div role="group" aria-label="Brain visualization" className="flex shrink-0 flex-wrap items-center gap-1 border-b border-edge-subtle px-3 py-1">
      <span className="mr-2 text-2xs text-text-muted">Canvas</span>
      {VIEWS.map(({ value, label }) => (
        <button
          key={value}
          type="button"
          aria-pressed={active === value}
          onClick={() => onSelect(value)}
          className={cn('td-hit border px-3 text-2xs', active === value ? 'border-accent bg-accent/10 text-text-primary' : 'border-edge-subtle text-text-secondary hover:bg-surface-2')}
        >
          {label}
        </button>
      ))}
    </div>
  );
}
