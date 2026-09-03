import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ObservedFamilyLedger } from './ObservedFamilyLedger.tsx';

afterEach(() => {
  vi.restoreAllMocks();
});

describe('the Observatory observation-family ledger', () => {
  it('makes the horizontally scrollable table viewport keyboard reachable', () => {
    stubHorizontalOverflow('auto');
    renderLedger();

    const viewport = screen.getByRole('region', {
      name: 'adoption observation counts table',
    });
    expect(viewport.getAttribute('tabindex')).toBe('0');
  });

  it('does not add a dead tab stop when the table viewport is not scrollable', () => {
    stubHorizontalOverflow('visible');
    renderLedger();

    const viewport = screen.getByRole('region', {
      name: 'adoption observation counts table',
    });
    expect(viewport.getAttribute('tabindex')).toBeNull();
  });
});

function renderLedger() {
  render(
    <ObservedFamilyLedger
      label="adoption observation counts"
      caption="Observed adoption-family records."
      marker="adoption"
      rows={[
        {
          eventKind: 'adoption.eligibility_observed.v1',
          label: 'eligibility observed',
          available: true,
          figure: '12',
          reason: null,
          state: 'ready',
          denominator: 'not published',
        },
      ]}
    />,
  );
}

function stubHorizontalOverflow(overflowX: 'auto' | 'visible') {
  const getComputedStyle = window.getComputedStyle.bind(window);
  vi.spyOn(window, 'getComputedStyle').mockImplementation((element, pseudoElement) => {
    const style = getComputedStyle(element, pseudoElement);
    Object.defineProperty(style, 'overflowX', { configurable: true, value: overflowX });
    return style;
  });
}
