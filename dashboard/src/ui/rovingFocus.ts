import type { KeyboardEvent } from 'react';

/**
 * Roving arrow keys over a list of native buttons.
 *
 * Rows are buttons, so Enter and Space activate for free; this moves focus
 * between them with the arrow, Home, End and Page keys so a reader never has
 * to Tab through every row. Returns `true` when the key was consumed, so a
 * caller can layer its own bindings over the ones that fell through.
 */
export function moveRovingFocus(
  container: HTMLElement | null,
  event: KeyboardEvent,
  selector = 'button',
  page = 10,
): boolean {
  if (!container) return false;
  const rows = [...container.querySelectorAll<HTMLElement>(selector)].filter(
    (row) => !row.hasAttribute('disabled'),
  );
  if (rows.length === 0) return false;
  const active = document.activeElement;
  const current = active instanceof HTMLElement ? rows.indexOf(active) : -1;
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
      return false;
  }
  event.preventDefault();
  rows[next]?.focus();
  rows[next]?.scrollIntoView({ block: 'nearest' });
  return true;
}
