import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { MeterRow } from './instrument.tsx';

/**
 * The distribution row five workspace plates had each rebuilt. Two of its
 * properties are load-bearing rather than cosmetic: a row whose share of the
 * whole is unknown draws no length, and the rail carries exactly one height
 * class, so its size does not depend on which utility the stylesheet happens to
 * emit last.
 */
describe('MeterRow', () => {
  it('draws no length when the share is unknown', () => {
    const { container } = render(<MeterRow label="tracedecay_search" value="—" fraction={null} />);

    const meter = container.querySelector('.td-meter');
    expect(meter).not.toBeNull();
    expect(meter?.querySelector('.td-meter-fill')).toBeNull();
  });

  it('prints the name and the figure, and gives the figure a length', () => {
    const { container, getByText } = render(
      <MeterRow label="tool_calls" value="1,945" fraction={0.25} />,
    );

    expect(getByText('tool_calls')).toBeTruthy();
    expect(getByText('1,945')).toBeTruthy();
    expect(container.querySelector<HTMLElement>('.td-meter-fill')?.style.width).toBe('25%');
    // The rail restates a number printed beside it, so it is redundant to a
    // screen reader rather than an unlabelled image.
    expect(container.querySelector('.td-meter')?.getAttribute('aria-hidden')).toBe('true');
  });
});
