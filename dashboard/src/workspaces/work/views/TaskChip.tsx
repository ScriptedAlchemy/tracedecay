import type { ButtonHTMLAttributes, ReactNode } from 'react';
import { cn } from '../../../ui/cn.ts';

/**
 * Shared task-selection chip for the Work projections.
 *
 * Every camera that can move the canonical selection uses this control so the
 * 44px target, `aria-pressed`, and `data-work-task` hook stay one contract.
 * 44px is spelled explicitly: the app's root font size is 14px, so `min-h-11`
 * computes to 38.5px and lands under the size the accessibility gate measures.
 *
 * `filled` is a claimed task (workload member, DAG node, causal endpoint).
 * `hollow` is an absence or a zero-extent reading (unattributed, unwoven,
 * landing): outline only, never a fill that would invent a measured span.
 */
export type TaskChipVariant = 'filled' | 'hollow';

export function TaskChip({
  taskId,
  selected,
  onSelect,
  variant,
  children,
  className,
  lamp = false,
  ...rest
}: {
  taskId: string;
  selected: boolean;
  onSelect: (taskId: string) => void;
  variant: TaskChipVariant;
  children: ReactNode;
  className?: string;
  /** Leading-edge lamp used by hollow zero-extent marks so selection is not a fill. */
  lamp?: boolean;
} & Omit<
  ButtonHTMLAttributes<HTMLButtonElement>,
  'type' | 'onClick' | 'onSelect' | 'aria-pressed'
>) {
  return (
    <button
      type="button"
      onClick={() => onSelect(taskId)}
      aria-pressed={selected}
      className={cn(
        'flex min-h-[44px] min-w-0 flex-col justify-center gap-0.5 border px-2 py-1 text-left',
        'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent',
        lamp && 'relative',
        variantClass(variant, selected),
        className,
      )}
      data-work-task={taskId}
      {...rest}
    >
      {lamp && selected ? (
        <span aria-hidden className="absolute inset-y-0 left-0 w-[2px] bg-accent" />
      ) : null}
      {children}
    </button>
  );
}

function variantClass(variant: TaskChipVariant, selected: boolean): string {
  switch (variant) {
    case 'filled':
      return selected
        ? 'border-accent bg-surface-3'
        : 'border-edge-subtle bg-surface-1 hover:bg-surface-2';
    case 'hollow':
      return selected ? 'border-accent bg-transparent' : 'bg-transparent';
    default: {
      const unhandled: never = variant;
      return unhandled;
    }
  }
}
