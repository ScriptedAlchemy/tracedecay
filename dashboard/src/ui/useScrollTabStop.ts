import { useEffect, useState, type RefObject } from 'react';

/**
 * The `tabIndex` for a region that is only sometimes a scroll container.
 *
 * A scrollable region has to be keyboard-operable (WCAG 2.1.1), and when its
 * contents hold nothing focusable — a read-out with no buttons in it — the
 * region itself has to take the tab stop, because there is nothing inside to
 * tab to. That is why several panels here carry `tabIndex={0}`.
 *
 * The reverse is also true and is what this exists for: a region that is NOT a
 * scroll container has no such need, and a tab stop on it is a stop that does
 * nothing. Panels whose overflow is applied at a breakpoint (`lg:overflow-auto`)
 * are both things at different widths, so a literal `tabIndex={0}` gives every
 * keyboard user on a narrow screen a dead stop in front of the content — at the
 * width where tabbing is most of the navigation.
 *
 * So the answer is measured rather than assumed. Reading the computed
 * `overflow-y` means the breakpoint lives in the stylesheet only; a JS copy of
 * `1024px` would be a second source of truth to keep in step. Under jsdom
 * nothing has layout or styles, so this reports "not a scroller" and the
 * behaviour is proved in a browser.
 */
export function useScrollTabStop(ref: RefObject<HTMLElement | null>): 0 | undefined {
  const [scrolls, setScrolls] = useState(false);
  useEffect(() => {
    const node = ref.current;
    if (node === null) return;
    const measure = () => {
      const overflow = getComputedStyle(node).overflowY;
      setScrolls(overflow === 'auto' || overflow === 'scroll');
    };
    measure();
    // Observing the element rather than the window: a breakpoint crossing
    // changes its box, and so does a layout change that has nothing to do with
    // the viewport.
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [ref]);
  return scrolls ? 0 : undefined;
}
