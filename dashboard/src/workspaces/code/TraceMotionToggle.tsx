/**
 * The motion control, on the surface that the motion is felt on.
 *
 * `reducedMotion.ts` owns the preference and its persistence; this owns only
 * the three-way control that sets it. Separating them is what makes the pinned
 * choice available to any surface while the control stays where a reader who
 * just felt a spring can reach it. `System` prints what the OS is currently
 * reporting, so choosing it is not a choice made blind.
 */
import { Gauge } from 'lucide-react';

import { cn } from '../../ui/cn';
import type { MotionPreference } from '../../viz/trace/reducedMotion.ts';

const MOTION_OPTIONS: ReadonlyArray<{ value: MotionPreference; label: string }> = [
  { value: 'system', label: 'System' },
  { value: 'full', label: 'Full' },
  { value: 'reduced', label: 'Reduced' },
];

export function TraceMotionToggle({
  preference,
  reduced,
  onChange,
}: {
  preference: MotionPreference;
  reduced: boolean;
  onChange: (next: MotionPreference) => void;
}) {
  return (
    <div
      role="radiogroup"
      aria-label="Motion"
      className="flex shrink-0 items-center overflow-hidden rounded-[var(--radius-standard)] border border-edge-subtle"
    >
      {MOTION_OPTIONS.map((option) => {
        const active = preference === option.value;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={active}
            onClick={() => onChange(option.value)}
            className={cn(
              'px-2 py-0.5 text-3xs',
              active
                ? 'bg-surface-2 text-text-primary'
                : 'bg-surface-0 text-text-muted hover:text-text-primary',
            )}
          >
            {option.value === 'system' && reduced ? `${option.label} · reduced` : option.label}
          </button>
        );
      })}
      <Gauge aria-hidden size={11} className="mx-1.5 shrink-0 text-text-muted" />
    </div>
  );
}
