import { describe, it, expect } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { StateChip, type DomainStateKind } from './StateChip';

function chipVisual(kind: DomainStateKind) {
  const { container } = render(<StateChip kind={kind} />);
  const chip = container.querySelector(`[data-state="${kind}"]`);
  expect(chip, `chip for ${kind}`).not.toBeNull();
  const glyph = chip!.querySelector('svg');
  const lamp = chip!.querySelector('span[aria-hidden]');
  expect(glyph, `icon for ${kind}`).not.toBeNull();
  expect(lamp, `lamp for ${kind}`).not.toBeNull();
  return { label: chip!.textContent, glyph: glyph!.innerHTML, lamp: lamp!.className };
}

describe('StateChip', () => {
  /**
   * The two near-neighbours a reader must never confuse: a reachable authority
   * reporting that one source cannot answer, and nothing being reachable at
   * all. They share a hue deliberately, both mean no reading arrived, so the
   * separation has to be carried by label, glyph and `data-state`. Asserting
   * the shared lamp alongside them is the point: if the colour were ever made
   * to do the work, this test would still hold the chip to saying it in a form
   * that survives colour blindness and monochrome.
   */
  it('tells a source that cannot answer apart from an unreachable daemon', () => {
    const unavailable = chipVisual('unavailable');
    const offline = chipVisual('offline');
    cleanup();

    expect(unavailable.label).toBe('Source unavailable');
    expect(offline.label).toBe('Offline');
    expect(unavailable.glyph).not.toBe(offline.glyph);
    expect(unavailable.lamp).toBe(offline.lamp);
  });

  /**
   * The other deliberate near-neighbour pair: a source that answered with
   * less than everything, and a provider quota that paused the read. Both
   * mean the shown evidence is real and incomplete, so they share a hue,
   * but a rate limit has its own remedy (wait for the reset), so it must be
   * tellable from `partial` by label and glyph, never only by detail text.
   */
  it('tells a rate-limited read apart from an ordinary partial answer', () => {
    const rateLimited = chipVisual('rate_limited');
    const partial = chipVisual('partial');
    cleanup();

    expect(rateLimited.label).toBe('Rate limited');
    expect(partial.label).toBe('Partial');
    expect(rateLimited.glyph).not.toBe(partial.glyph);
    expect(rateLimited.lamp).toBe(partial.lamp);
  });

  it('renders an optional detail suffix alongside the label', () => {
    expect(chipVisual('stale').label).toBe('Stale');
    cleanup();
    render(<StateChip kind="stale" detail="12m ago" />);
    expect(screen.getByText('Stale').parentElement?.textContent).toBe('Stale· 12m ago');
  });
});
