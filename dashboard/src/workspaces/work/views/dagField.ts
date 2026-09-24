import type { KeyboardEvent } from 'react';
import type { WorkDagLayout } from '../workDagLayout.ts';
import type { WorkTaskView } from '../workProductView.ts';
import type { WorkDagReading } from '../workViewsModel.ts';

/**
 * What the dependency board hands each of its fields: the fitted graph and
 * the matrix read one layout and share the board's inspect/select grammar.
 */
export type WorkDagFieldView = 'graph' | 'matrix';

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
