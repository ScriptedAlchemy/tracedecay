import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { CapacityBar } from './ActivityColumns.tsx';

/**
 * The only quantity this bar encodes is the free-page share, so an unreported
 * free figure leaves it with nothing to draw. It used to default that figure to
 * zero, which drew a completely filled bar announcing "0.0% free pages" — a
 * store measured as full — for a store whose pages nobody sampled. The caller
 * that hit it worked around it and left the defect in the primitive, so the
 * honest state belongs here, where the next caller inherits it.
 */
describe('CapacityBar', () => {
  it('says the free pages are unknown instead of drawing a full bar', () => {
    const { container, getByText, queryByRole } = render(
      <CapacityBar usedBytes={645_120_000} freeBytes={null} />,
    );

    expect(getByText('free pages unknown')).toBeTruthy();
    // No bar at all: neither the fill that would read as a used fraction nor
    // the "0.0% free pages" label that would read as a measurement.
    expect(queryByRole('img')).toBeNull();
    expect(container.querySelector('svg')).toBeNull();
  });

  it('says the size is unknown when there is no total', () => {
    const { getByText } = render(<CapacityBar usedBytes={null} freeBytes={4_096} />);
    expect(getByText('size unknown')).toBeTruthy();
  });

  it('draws and announces the measured free share when both figures arrive', () => {
    const { getByRole } = render(<CapacityBar usedBytes={1_000} freeBytes={250} />);
    expect(getByRole('img').getAttribute('aria-label')).toBe(
      'store size with 25.0% free pages',
    );
  });

  it('gives every instance its own hatch pattern', () => {
    // The observatory draws one bar per store card. A fixed pattern id made
    // every `url(#…)` in the document resolve against the first bar's hatch.
    const { container } = render(
      <>
        <CapacityBar usedBytes={1_000} freeBytes={250} />
        <CapacityBar usedBytes={1_000} freeBytes={500} />
      </>,
    );

    const ids = [...container.querySelectorAll('pattern')].map((pattern) => pattern.id);
    expect(ids).toHaveLength(2);
    expect(new Set(ids).size).toBe(2);
    expect([...container.querySelectorAll('rect')].map((rect) => rect.getAttribute('fill'))).toEqual(
      ids.map((id) => `url(#${id})`),
    );
  });
});
