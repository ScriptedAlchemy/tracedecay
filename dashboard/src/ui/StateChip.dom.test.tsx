import { describe, it, expect } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { StateChip, type DomainStateKind } from './StateChip';

/**
 * Example DOM test (plan 11a R56 / plan 11 R64): every domain state renders a
 * non-color-alone chip — an icon *and* a text label. The `Record` type below is
 * the compile-time exhaustiveness gate: if the taxonomy in StateChip.tsx gains
 * or drops a state, tsc fails here until this table is updated, so the "all
 * states" claim can never silently rot.
 */
const EXPECTED_LABELS: Record<DomainStateKind, string> = {
  loading: 'Loading',
  complete_zero_findings: 'Complete · zero findings',
  ready: 'Ready',
  partial: 'Partial',
  stale: 'Stale',
  locked: 'Locked',
  denied: 'Denied',
  unauthorized: 'Unauthorized',
  redacted: 'Redacted',
  conflicting: 'Conflicting',
  offline: 'Offline',
  unknown: 'Unknown',
  cancelled: 'Cancelled',
  timed_out: 'Timed out',
  error: 'Error',
  unsupported: 'Unsupported',
  unsupported_schema: 'Unsupported schema',
};

const ENTRIES = Object.entries(EXPECTED_LABELS) as [DomainStateKind, string][];

describe('StateChip', () => {
  it('covers exactly 17 domain states', () => {
    expect(ENTRIES).toHaveLength(17);
  });

  it.each(ENTRIES)('renders icon + label for "%s"', (kind, label) => {
    const { container } = render(<StateChip kind={kind} />);

    const chip = container.querySelector(`[data-state="${kind}"]`);
    expect(chip, `chip for ${kind}`).not.toBeNull();

    // Icon: lucide renders an inline <svg> (aria-hidden) — never color alone.
    expect(chip!.querySelector('svg'), `icon for ${kind}`).not.toBeNull();

    // Label: the human-readable text is present and exact.
    expect(screen.getByText(label)).toBeTruthy();

    cleanup();
  });

  it('renders an optional detail suffix alongside the label', () => {
    render(<StateChip kind="stale" detail="12m ago" />);
    expect(screen.getByText('Stale')).toBeTruthy();
    expect(screen.getByText(/12m ago/)).toBeTruthy();
  });
});
