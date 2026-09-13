import {
  Fragment,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { cn } from './cn';

/** Row list virtualization. Below the threshold the list renders
 * plainly — byte-identical DOM to a bare `.map()` so the common case keeps the
 * archetype's exact scroll/selection idiom. Above it, rows are windowed with
 * @tanstack/react-virtual so the mounted count stays bounded (a 36px row over
 * even a very tall viewport plus overscan mounts well under 250 rows) instead
 * of scaling with the result count. Rows are the page's own components, passed
 * through `renderItem`; this helper only owns containment. */

/** Result rows switch to windowing above this many entries. */
const VIRTUALIZE_THRESHOLD = 200;
/** DataRow is a fixed-height button that never wraps, so a single fixed
 * estimate positions every row exactly with no measurement pass — provided the
 * estimate matches the row. It is read from `--row-height-data`, the same token
 * DataRow sizes itself from, because a hard-coded copy had already drifted 4px
 * away from the real row height and offset every windowed row. */
const ROW_HEIGHT_FALLBACK = 36;

function rowHeight(): number {
  if (typeof window === 'undefined' || typeof getComputedStyle !== 'function') {
    return ROW_HEIGHT_FALLBACK;
  }
  const raw = getComputedStyle(document.documentElement)
    .getPropertyValue('--row-height-data')
    .trim();
  const parsed = Number.parseFloat(raw);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : ROW_HEIGHT_FALLBACK;
}

export interface VirtualListProps<T> {
  items: T[];
  renderItem: (item: T, index: number) => ReactNode;
  getKey: (item: T, index: number) => string | number;
  /** Optional non-virtualized content pinned above the rows (e.g. a count
   * strip). Scrolls with the list, matching the current inline-header idiom. */
  header?: ReactNode;
  estimateHeight?: number;
  overscan?: number;
  threshold?: number;
  className?: string;
}

export function VirtualList<T>({
  items,
  renderItem,
  getKey,
  header,
  estimateHeight,
  overscan = 12,
  threshold = VIRTUALIZE_THRESHOLD,
  className,
}: VirtualListProps<T>) {
  if (items.length <= threshold) {
    return (
      <div className={className}>
        {header}
        {items.map((item, index) => (
          <Fragment key={getKey(item, index)}>{renderItem(item, index)}</Fragment>
        ))}
      </div>
    );
  }
  return (
    <VirtualRows
      items={items}
      renderItem={renderItem}
      getKey={getKey}
      header={header}
      estimateHeight={estimateHeight ?? rowHeight()}
      overscan={overscan}
      className={className}
    />
  );
}

function VirtualRows<T>({
  items,
  renderItem,
  getKey,
  header,
  estimateHeight,
  overscan,
  className,
}: {
  items: T[];
  renderItem: (item: T, index: number) => ReactNode;
  getKey: (item: T, index: number) => string | number;
  header?: ReactNode;
  estimateHeight: number;
  overscan: number;
  className?: string;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // Offset of the row area within the scroll container (the header height), so
  // window math stays correct when a header is pinned above the rows.
  const [scrollMargin, setScrollMargin] = useState(0);

  useLayoutEffect(() => {
    setScrollMargin(listRef.current?.offsetTop ?? 0);
  }, [header, items.length]);

  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => estimateHeight,
    overscan,
    scrollMargin,
    getItemKey: (index) => {
      const item = items[index];
      return item === undefined ? index : getKey(item, index);
    },
  });

  return (
    <div ref={scrollRef} className={cn('relative h-full overflow-auto', className)}>
      {header}
      <div ref={listRef}>
        <div
          style={{
            height: virtualizer.getTotalSize(),
            width: '100%',
            position: 'relative',
          }}
        >
          {virtualizer.getVirtualItems().map((row) => {
            const item = items[row.index];
            if (item === undefined) return null;
            return (
              <div
                key={row.key}
                style={{
                  position: 'absolute',
                  top: 0,
                  left: 0,
                  width: '100%',
                  transform: `translateY(${row.start - scrollMargin}px)`,
                }}
              >
                {renderItem(item, row.index)}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
