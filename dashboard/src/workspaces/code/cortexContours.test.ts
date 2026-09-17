/**
 * Side-by-side comparison of the three contour sketches. The committed
 * encoding has to be the unique winner; a second formula for the same
 * question is not another candidate.
 */
import { describe, expect, it } from 'vitest';

import {
  COMMITTED_CONTOUR_ENCODING,
  CONTOUR_ENCODINGS,
  independentOfFileMass,
  separatesEqualMassLeakage,
  sketchContour,
  statesSealedWithoutARatio,
  winningContourEncoding,
} from './cortexContours.ts';

describe('contour encoding comparison', () => {
  it('commits coupling ratio because it is the only sketch that passes every witness', () => {
    expect(winningContourEncoding()).toBe('coupling_ratio');
    expect(COMMITTED_CONTOUR_ENCODING).toBe(winningContourEncoding());
  });

  it('rejects edges-per-file: equal mass and equal internal edges share one ring count', () => {
    expect(separatesEqualMassLeakage('edges_per_file')).toBe(false);
    const leaky = sketchContour('edges_per_file', {
      files: 40,
      internalEdges: 80,
      boundaryEdges: 80,
    });
    const closed = sketchContour('edges_per_file', {
      files: 40,
      internalEdges: 80,
      boundaryEdges: 10,
    });
    expect(leaky).toEqual(closed);
  });

  it('rejects per-file rates: growing file mass restates area on the rings', () => {
    for (const encoding of ['edges_per_file', 'boundary_per_file'] as const) {
      expect(independentOfFileMass(encoding)).toBe(false);
    }
    expect(independentOfFileMass('coupling_ratio')).toBe(true);
    expect(separatesEqualMassLeakage('coupling_ratio')).toBe(true);
    expect(statesSealedWithoutARatio('coupling_ratio')).toBe(true);
  });

  it('does not invent a finite ratio when the boundary measured zero', () => {
    expect(
      sketchContour('coupling_ratio', { files: 4, internalEdges: 8, boundaryEdges: 0 }),
    ).toEqual({ kind: 'sealed' });
    for (const encoding of CONTOUR_ENCODINGS) {
      if (encoding === 'coupling_ratio') continue;
      expect(statesSealedWithoutARatio(encoding)).toBe(false);
    }
  });
});
