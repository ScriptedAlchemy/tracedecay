import { render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { VirtualList } from './VirtualList.tsx';

/**
 * Plan 11's list bound: "Virtualization starts above 200 rows, mounts at most
 * 250 row-like elements plus one inspector, preserves focused/selected
 * entities, and always offers a nonvirtualized paginated mode of at most 100
 * rows."
 *
 * The mount ceiling was previously only a comment in `VirtualList.tsx`. A
 * comment does not fail, and the ceiling is what keeps a large result set from
 * becoming thousands of DOM nodes, so it is asserted against the real
 * virtualizer here.
 *
 * jsdom performs no layout: every element measures zero, and a virtualizer
 * given a zero-height viewport mounts nothing at all — which would satisfy any
 * ceiling by drawing an empty list. So each windowed case installs a measured
 * viewport and also asserts a nonzero mount strictly below the item count.
 */

/** Plan 11: at most 250 row-like elements mounted at once. */
const MOUNT_CEILING = 250;

/**
 * Give jsdom a viewport the virtualizer can measure.
 *
 * TanStack Virtual sizes the scroll element with `offsetWidth`/`offsetHeight`
 * (`getRect` in `@tanstack/virtual-core`), which jsdom always reports as zero —
 * not `clientHeight` or `getBoundingClientRect`. It also needs a
 * `ResizeObserver` to exist on the window, which jsdom does not provide.
 */
function installViewport(height: number, width = 900): () => void {
  const names = ['offsetHeight', 'offsetWidth'] as const;
  const saved = names.map(
    (name) => [name, Object.getOwnPropertyDescriptor(HTMLElement.prototype, name)] as const,
  );
  Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
    configurable: true,
    get: () => height,
  });
  Object.defineProperty(HTMLElement.prototype, 'offsetWidth', {
    configurable: true,
    get: () => width,
  });

  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    },
  );

  return () => {
    for (const [name, descriptor] of saved) {
      if (descriptor) Object.defineProperty(HTMLElement.prototype, name, descriptor);
      else delete (HTMLElement.prototype as unknown as Record<string, unknown>)[name];
    }
  };
}

let restore: (() => void) | undefined;

afterEach(() => {
  restore?.();
  restore = undefined;
  vi.unstubAllGlobals();
});

function rows(count: number): string[] {
  return Array.from({ length: count }, (_, index) => `row-${index}`);
}

function renderRows(count: number) {
  const view = render(
    <VirtualList
      items={rows(count)}
      getKey={(item) => item}
      renderItem={(item) => (
        <button type="button" data-row={item}>
          {item}
        </button>
      )}
    />,
  );
  return {
    ...view,
    mounted: () => view.container.querySelectorAll('[data-row]').length,
  };
}

describe('VirtualList row bounds', () => {
  it('renders a 100-row page without windowing it', () => {
    // The plan's escape hatch from virtualization is a paginated mode of at
    // most 100 rows, so that size stays plainly rendered — the same DOM as a
    // bare map, with every row present for find-in-page and screen readers.
    expect(renderRows(100).mounted()).toBe(100);
  });

  it('still renders every row at the 200-row threshold', () => {
    expect(renderRows(200).mounted()).toBe(200);
  });

  it('windows above the threshold and stays under the mount ceiling', () => {
    restore = installViewport(900);
    const count = 5_000;
    const { mounted } = renderRows(count);

    expect(mounted()).toBeGreaterThan(0);
    expect(mounted()).toBeLessThan(count);
    expect(mounted()).toBeLessThanOrEqual(MOUNT_CEILING);
  });

  it('holds the ceiling on a viewport far taller than any supported tier', () => {
    // 2000 CSS pixels is well past the tallest tier the plan measures, so a
    // ceiling that holds here is not an artifact of a small test viewport.
    restore = installViewport(2_000);
    const { mounted } = renderRows(20_000);

    expect(mounted()).toBeGreaterThan(0);
    expect(mounted()).toBeLessThanOrEqual(MOUNT_CEILING);
  });

  it('does not scale mounted rows with the result count', () => {
    restore = installViewport(900);
    const small = renderRows(1_000).mounted();
    const large = renderRows(20_000).mounted();

    // The property that matters: a twentyfold larger corpus must not mount
    // twentyfold more rows. Both are viewport-driven, so they match.
    expect(small).toBeGreaterThan(0);
    expect(large).toBeLessThanOrEqual(small);
  });

  it('preserves a row element across a re-render with the same items', () => {
    restore = installViewport(900);
    const items = rows(5_000);
    const renderItem = (item: string) => (
      <button type="button" data-row={item}>
        {item}
      </button>
    );
    const { container, rerender } = render(
      <VirtualList items={items} getKey={(item) => item} renderItem={renderItem} />,
    );
    const before = container.querySelector('[data-row="row-0"]');
    expect(before).toBeTruthy();

    rerender(<VirtualList items={items} getKey={(item) => item} renderItem={renderItem} />);

    // Stable keys, so React reuses the node rather than remounting it — which
    // is what lets focus and selection survive a refetch.
    expect(container.querySelector('[data-row="row-0"]')).toBe(before);
  });
});
