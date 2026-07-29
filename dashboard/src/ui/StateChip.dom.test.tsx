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
  unavailable: 'Source unavailable',
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
  it('covers exactly 18 domain states', () => {
    expect(ENTRIES).toHaveLength(18);
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

  /**
   * The two near-neighbours a reader must never confuse: a reachable authority
   * reporting that one source cannot answer, and nothing being reachable at
   * all. They share a hue deliberately — both mean no reading arrived — so the
   * separation has to be carried by label, glyph and `data-state`. Asserting
   * the shared lamp alongside them is the point: if the colour were ever made
   * to do the work, this test would still hold the chip to saying it in a form
   * that survives colour blindness and monochrome.
   */
  it('tells a source that cannot answer apart from an unreachable daemon', () => {
    const chipFor = (kind: DomainStateKind) => {
      const { container } = render(<StateChip kind={kind} />);
      const chip = container.querySelector(`[data-state="${kind}"]`);
      expect(chip, `chip for ${kind}`).not.toBeNull();
      const glyph = chip!.querySelector('svg');
      const lamp = chip!.querySelector('span[aria-hidden]');
      expect(glyph, `icon for ${kind}`).not.toBeNull();
      expect(lamp, `lamp for ${kind}`).not.toBeNull();
      return { label: chip!.textContent, glyph: glyph!.innerHTML, lamp: lamp!.className };
    };

    const unavailable = chipFor('unavailable');
    const offline = chipFor('offline');
    cleanup();

    expect(unavailable.label).toBe('Source unavailable');
    expect(offline.label).toBe('Offline');
    expect(unavailable.glyph).not.toBe(offline.glyph);
    expect(unavailable.lamp).toBe(offline.lamp);
  });

  it('renders an optional detail suffix alongside the label', () => {
    render(<StateChip kind="stale" detail="12m ago" />);
    expect(screen.getByText('Stale')).toBeTruthy();
    expect(screen.getByText(/12m ago/)).toBeTruthy();
  });
});
