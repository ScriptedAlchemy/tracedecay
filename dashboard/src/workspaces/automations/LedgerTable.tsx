import {
  useRef,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
} from 'react';

import { cn } from '../../ui/cn';
import type { Tone } from './ledger.ts';

/**
 * The ledger grammar shared by every table on the Automations surface.
 *
 *   LedgerTable  a semantic table with engraved column legends; it is the
 *                exact text fallback as well as the primary view
 *   InspectRow   hover or focus inspects, click or Enter selects — the
 *                design system's one interaction language, in a table row
 *   ToneWord     a typed-state word beside its lamp; the word is always
 *                printed, the lamp only says which family it is in
 *   Absent       a typed absence, printed as a reason rather than a blank
 */

/** A column: its engraved legend and the share of the table it is given.
 * Shares are percentages of the table width under `table-layout: fixed`, so
 * long identifiers wrap inside their column instead of widening it and
 * pushing the trailing columns out of the panel. */
export interface LedgerColumn {
  readonly label: string;
  readonly width: number;
}

export function LedgerTable({
  columns,
  caption,
  children,
  className,
  minWidth,
  onPointerLeave,
}: {
  columns: readonly LedgerColumn[];
  /** Screen-reader caption naming the table and its window. */
  caption: string;
  children: ReactNode;
  className?: string;
  /** Utility class fixing the table's floor width, below which the panel
   * scrolls horizontally rather than crushing every column. */
  minWidth?: string;
  /** Fired when the pointer leaves the table body — the moment a hover
   * preview should yield back to the pinned selection. */
  onPointerLeave?: () => void;
}) {
  const tableRef = useRef<HTMLTableElement>(null);
  // Roving arrows over the row buttons, so a keyboard reader moves through a
  // ledger without tabbing every cell. Enter/Space activate natively.
  const onKeyDown = (event: KeyboardEvent<HTMLTableElement>) => {
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp' && event.key !== 'Home' && event.key !== 'End') return;
    const table = tableRef.current;
    if (!table) return;
    const rows = [...table.querySelectorAll<HTMLButtonElement>('button[data-ledger-row]')];
    if (rows.length === 0) return;
    const current = rows.indexOf(document.activeElement as HTMLButtonElement);
    let next: number;
    switch (event.key) {
      case 'Home':
        next = 0;
        break;
      case 'End':
        next = rows.length - 1;
        break;
      case 'ArrowDown':
        next = Math.min(current + 1, rows.length - 1);
        break;
      default:
        next = Math.max(current - 1, 0);
    }
    event.preventDefault();
    rows[next]?.focus();
  };
  const leave = (event: PointerEvent<HTMLTableElement>) => {
    // A row that holds keyboard focus is being inspected on purpose; the
    // pointer wandering off must not take that inspection with it.
    if (event.currentTarget.contains(document.activeElement)) return;
    onPointerLeave?.();
  };
  return (
    <div className={cn('min-w-0 overflow-x-auto', className)}>
      <table
        ref={tableRef}
        onKeyDown={onKeyDown}
        onPointerLeave={leave}
        className={cn('w-full table-fixed border-collapse text-2xs', minWidth ?? 'min-w-[36rem]')}
      >
        <caption className="sr-only">{caption}</caption>
        <colgroup>
          {columns.map((column) => (
            <col key={column.label} style={{ width: `${column.width}%` }} />
          ))}
        </colgroup>
        <thead>
          <tr>
            {columns.map((column) => (
              <th
                key={column.label}
                scope="col"
                className="td-legend whitespace-normal border-b border-edge-subtle px-2 pb-1.5 pt-1 text-left leading-tight"
              >
                {column.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

/** One inspectable row. The identity cell carries the row's button; hover
 * anywhere on the row and focus on the button both inspect, click and
 * Enter/Space select. Selection is a cyan gutter plus `aria-pressed`, never a
 * glow. Rows keep the data-row height so the button meets the touch minimum. */
export function InspectRow({
  label,
  inspected,
  selected,
  onInspect,
  onSelect,
  identity,
  children,
  testId,
}: {
  /** Accessible name for the row's button: what selecting it inspects. */
  label: string;
  inspected: boolean;
  selected: boolean;
  onInspect: () => void;
  onSelect: () => void;
  /** The identity cell's content, rendered inside the button. */
  identity: ReactNode;
  /** The remaining cells, as `<td>` elements. */
  children: ReactNode;
  testId?: string;
}) {
  return (
    <tr
      data-testid={testId}
      data-inspected={inspected || undefined}
      data-selected={selected || undefined}
      onPointerEnter={onInspect}
      className={cn(
        'border-b border-edge-subtle last:border-b-0',
        // Hover raises the face one plane; the pinned row sits one higher so
        // preview and selection stay distinguishable while both are visible.
        'hover:bg-surface-1',
        inspected && !selected && 'bg-surface-1',
        selected && 'bg-surface-2',
      )}
    >
      <td className="relative p-0 align-middle">
        <span
          aria-hidden
          className={cn('absolute inset-y-0 left-0 w-[3px]', selected ? 'bg-accent' : 'bg-transparent')}
        />
        <button
          type="button"
          data-ledger-row
          aria-pressed={selected}
          aria-label={label}
          onFocus={onInspect}
          onClick={onSelect}
          className="flex min-h-[var(--row-height-data)] w-full min-w-0 items-center gap-2 px-2.5 text-left focus-visible:bg-surface-1"
        >
          {identity}
        </button>
      </td>
      {children}
    </tr>
  );
}

/** A plain data cell in a ledger row. */
export function Cell({
  children,
  className,
  numeric,
}: {
  children: ReactNode;
  className?: string;
  numeric?: boolean;
}) {
  return (
    <td
      data-cell={numeric ? 'numeric' : undefined}
      className={cn('break-words px-2 py-1 align-middle', numeric && 'td-value text-2xs', className)}
    >
      {children}
    </td>
  );
}

/** A status word beside its typed-state lamp. The lamp's shape reinforces
 * the family without colour: solid for ready and refusal, hatched for
 * degraded, dashed outline for disconnected and unknown. */
export function ToneWord({
  tone,
  word,
  className,
}: {
  tone: Tone;
  word: string;
  className?: string;
}) {
  return (
    <span className={cn('inline-flex min-w-0 items-baseline gap-1.5', className)}>
      <ToneLamp tone={tone} className="relative top-px" />
      <span className={cn('break-words', tone.text)}>{word}</span>
    </span>
  );
}

export function ToneLamp({ tone, className }: { tone: Tone; className?: string }) {
  switch (tone.pattern) {
    case 'hatched':
      return (
        <span
          aria-hidden
          className={cn('size-2 shrink-0', tone.text, className)}
          style={{ backgroundImage: 'var(--ev-associated)' }}
        />
      );
    case 'dashed':
      return (
        <span
          aria-hidden
          className={cn('size-2 shrink-0 border border-dashed border-current', tone.text, className)}
        />
      );
    case 'solid':
      return <span aria-hidden className={cn('size-2 shrink-0', tone.lamp, className)} />;
    default: {
      const exhaustive: never = tone.pattern;
      return exhaustive;
    }
  }
}

/** A typed absence. Printed as its reason, in the muted register, so a
 * missing value reads as "the daemon did not serve this" rather than as a
 * blank cell that might mean zero. */
export function Absent({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <span className={cn('text-text-muted', className)}>
      <span aria-hidden>— </span>
      {children}
    </span>
  );
}

/** A definition-list term for the inspector: engraved label above, value
 * below, wrapping so identifiers and reasons can be read in full. */
export function Term({
  label,
  children,
  mono = false,
}: {
  label: string;
  children: ReactNode;
  mono?: boolean;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="td-legend whitespace-normal leading-tight">{label}</dt>
      <dd className={cn('min-w-0 break-words text-2xs text-text-secondary', mono && 'td-value text-2xs')}>
        {children}
      </dd>
    </div>
  );
}
