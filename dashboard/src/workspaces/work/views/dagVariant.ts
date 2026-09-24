import { useCallback, type KeyboardEvent } from 'react';
import { useSearchParams } from 'react-router';
import type { WorkDagLayout } from '../workDagLayout.ts';
import type { WorkTaskView } from '../workProductView.ts';
import type { WorkDagReading } from '../workViewsModel.ts';

/**
 * The dependency board's renderers over one layout. `layered` is the board as
 * it shipped; the others re-read the same reading: fitted to the field, laid
 * into lanes, or folded into a dependency structure matrix. The renderer lives
 * in the address beside the camera and the selection, so a link reproduces
 * what the reader was looking at.
 */
export type WorkDagVariant = 'layered' | 'fitted' | 'swimlane' | 'matrix';

export const WORK_DAG_VARIANTS: readonly { readonly value: WorkDagVariant; readonly label: string }[] = [
  { value: 'layered', label: 'Layered' },
  { value: 'fitted', label: 'Fitted' },
  { value: 'swimlane', label: 'Swimlane' },
  { value: 'matrix', label: 'Matrix' },
];

export const DAG_VARIANT_PARAM = 'dag';

function asVariant(value: string | null): WorkDagVariant {
  return WORK_DAG_VARIANTS.find((variant) => variant.value === value)?.value ?? 'layered';
}

export function useDagVariant(): [WorkDagVariant, (next: WorkDagVariant) => void] {
  const [params, setParams] = useSearchParams();
  const active = asVariant(params.get(DAG_VARIANT_PARAM));
  const select = useCallback(
    (next: WorkDagVariant) => {
      const updated = new URLSearchParams(params);
      if (next === 'layered') updated.delete(DAG_VARIANT_PARAM);
      else updated.set(DAG_VARIANT_PARAM, next);
      setParams(updated, { replace: true });
    },
    [params, setParams],
  );
  return [active, select];
}

export interface Emphasis {
  readonly tasks: ReadonlySet<string>;
  readonly edges: ReadonlySet<string>;
}

/** Everything a card control needs from the board: its roving tab stop, its
 * registration for arrow traversal, and the inspect/select acts. */
export interface CardWiring {
  readonly tabIndex: number;
  readonly ref: (element: HTMLButtonElement | null) => void;
  readonly onClick: () => void;
  readonly onFocus: () => void;
  readonly onBlur: () => void;
  readonly onPointerEnter: () => void;
  readonly onKeyDown: (event: KeyboardEvent<HTMLButtonElement>) => void;
}

export interface DagFieldProps {
  readonly layout: WorkDagLayout;
  readonly reading: WorkDagReading;
  readonly tasks: ReadonlyMap<string, WorkTaskView>;
  readonly selected: string | null;
  readonly inspected: string | null;
  readonly isolation: Emphasis | null;
  readonly critical: Emphasis | null;
  readonly showLabels: boolean;
  readonly zoom: number;
  readonly onSelect: (taskId: string) => void;
  readonly onInspect: (taskId: string | null) => void;
  readonly card: (taskId: string) => CardWiring;
}
