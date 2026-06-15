import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Minimal uniform-row virtualization for long scrollable lists.
 *
 * Renders only the rows intersecting the scrollport (plus overscan) inside a
 * full-height spacer, so a list of a few thousand facts costs ~30 DOM rows.
 */
export function useVirtualList({
  count,
  rowHeight,
  overscan = 8,
}: {
  count: number;
  rowHeight: number;
  overscan?: number;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [range, setRange] = useState<[number, number]>([0, Math.min(count, 40)]);
  const frameRef = useRef(0);

  const recompute = useCallback(() => {
    const node = containerRef.current;
    if (!node) return;
    const start = Math.max(0, Math.floor(node.scrollTop / rowHeight) - overscan);
    const visible = Math.ceil(node.clientHeight / rowHeight) + overscan * 2;
    const end = Math.min(count, start + visible);
    setRange((prev) => (prev[0] === start && prev[1] === end ? prev : [start, end]));
  }, [count, rowHeight, overscan]);

  useEffect(() => {
    recompute();
  }, [recompute]);

  const onScroll = useCallback(() => {
    cancelAnimationFrame(frameRef.current);
    frameRef.current = requestAnimationFrame(recompute);
  }, [recompute]);

  useEffect(() => () => cancelAnimationFrame(frameRef.current), []);

  return {
    containerRef,
    onScroll,
    start: range[0],
    end: range[1],
    totalHeight: count * rowHeight,
    offsetTop: range[0] * rowHeight,
  };
}
