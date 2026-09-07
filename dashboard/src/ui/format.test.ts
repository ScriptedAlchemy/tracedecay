import { describe, expect, it } from 'vitest';
import { formatCount, splitBytes, splitCount, splitSignedBytes } from './format.ts';

/**
 * The magnitude language is shared so two surfaces cannot quietly abbreviate the
 * same number differently. Where a surface genuinely needs a different scale —
 * a token ledger whose small end is already four figures — that is a parameter
 * here, not a second copy of the thresholds.
 */
describe('count thresholds', () => {
  it('prints four figures in full by default', () => {
    expect(formatCount(9_842)).toBe((9_842).toLocaleString());
    expect(formatCount(12_800)).toBe('12.8k');
  });

  it('abbreviates from the caller-chosen magnitude', () => {
    expect(formatCount(9_842, 1_000)).toBe('9.8k');
    expect(formatCount(999, 1_000)).toBe('999');
  });

  it('splits on the same threshold', () => {
    expect(splitCount(9_842)).toEqual({ value: (9_842).toLocaleString() });
    expect(splitCount(9_842, 1_000)).toEqual({ value: '9.8', unit: 'K' });
  });

  it('leaves the millions and billions tiers where they are', () => {
    expect(formatCount(2_400_000, 1_000)).toBe('2.4M');
    expect(splitCount(1_200_000_000, 1_000)).toEqual({ value: '1.2', unit: 'B' });
  });

  it('answers an absent count with an em dash, never a zero', () => {
    expect(formatCount(null, 1_000)).toBe('—');
    expect(splitCount(undefined, 1_000)).toEqual({ value: '—' });
  });
});

describe('splitSignedBytes', () => {
  it('reads a shrink as a shrink and leaves growth bare', () => {
    expect(splitSignedBytes(-1_536)).toEqual({ value: '-1.5', unit: 'KiB' });
    expect(splitSignedBytes(1_536)).toEqual({ value: '1.5', unit: 'KiB' });
  });

  it('speaks the same magnitudes as splitBytes', () => {
    expect(splitSignedBytes(645_120_000)).toEqual(splitBytes(645_120_000));
    expect(splitSignedBytes(-645_120_000).unit).toBe(splitBytes(645_120_000).unit);
  });

  it('leaves an unchanged figure unsigned', () => {
    expect(splitSignedBytes(0)).toEqual({ value: '0', unit: 'B' });
  });

  it('answers an unreported delta with an em dash', () => {
    expect(splitSignedBytes(null)).toEqual({ value: '—' });
    expect(splitSignedBytes(Number.NaN)).toEqual({ value: '—' });
  });
});
