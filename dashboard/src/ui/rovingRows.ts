import type { KeyboardEvent } from 'react';

/**
 * Roving arrows over a list of row buttons.
 *
 * Rows are native `<button>`s, so Enter/Space activate for free; this adds the
 * movement keys, arrows, Home, End and Page keys, so a keyboard reader can
 * move through a result list without tabbing every row. The container's
 * `onKeyDown` hands its element and the event here; the handler moves focus
 * and consumes the key only when it recognised one.
 */
export function rovingRowsKeyDown(container: HTMLElement | null, event: KeyboardEvent): void {
  if (!container) return;
  const rows = [...container.querySelectorAll<HTMLButtonElement>('button')];
  if (rows.length === 0) return;
  const active = document.activeElement;
  const current = active instanceof HTMLButtonElement ? rows.indexOf(active) : -1;
  const page = 10;
  const last = rows.length - 1;
  const from = current < 0 ? 0 : current;
  let next: number;
  switch (event.key) {
    case 'Home':
      next = 0;
      break;
    case 'End':
      next = last;
      break;
    case 'PageDown':
      next = Math.min(from + page, last);
      break;
    case 'PageUp':
      next = Math.max(from - page, 0);
      break;
    case 'ArrowDown':
      next = Math.min(current + 1, last);
      break;
    case 'ArrowUp':
      next = Math.max(current - 1, 0);
      break;
    default:
      return;
  }
  event.preventDefault();
  rows[next]?.focus();
  rows[next]?.scrollIntoView({ block: 'nearest' });
}
