import { useLayoutEffect, useRef, useState } from "react";

/**
 * Track the rendered width of a container so SVG charts can fill the
 * available space and reflow on viewport/sidebar resizes.
 */
export function useMeasuredWidth<T extends HTMLElement>(
  initial = 720,
): [React.RefObject<T | null>, number] {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(initial);

  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return;
    const observer = new ResizeObserver((entries) => {
      const measured = Math.floor(entries[0].contentRect.width);
      if (measured > 0) {
        setWidth((prev) => (Math.abs(prev - measured) > 1 ? measured : prev));
      }
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, []);

  return [ref, width];
}
