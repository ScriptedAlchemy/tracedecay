/**
 * Competing contour encodings for the CORTEX relief, compared before commit.
 *
 * Area already carries file mass and elevation already carries dependency
 * depth. The ring channel was shipped as internal edges per file while the
 * round-one sheet still named coupling ratio (internal ÷ boundary) as the
 * unread alternative. These three sketches are different questions, not
 * recolors of one formula:
 *
 *   edges_per_file     how busy is each file inside the region
 *   boundary_per_file  how much does each file leak across the region edge
 *   coupling_ratio     how closed is the region relative to that edge
 *
 * `internal / (internal + boundary)` is the same closure question as
 * `coupling_ratio` with a bounded range. It is not a third shape, so it is
 * not sketched. The committed encoding is the unique sketch that separates
 * equal-mass leakage, stays put when only file mass changes, and refuses to
 * invent a finite ratio for a sealed region.
 */
export const CONTOUR_INTERVAL = 0.5;
/** Rings a region can carry before the interior stops being readable. */
export const MAX_DRAWN_CONTOURS = 9;

export const CONTOUR_ENCODINGS = [
  'edges_per_file',
  'boundary_per_file',
  'coupling_ratio',
] as const;
export type ContourEncodingId = (typeof CONTOUR_ENCODINGS)[number];

/** The encoding the comparison commits. Tests fail if it stops being the winner. */
export const COMMITTED_CONTOUR_ENCODING = 'coupling_ratio' as const;

export interface ContourSample {
  readonly files: number;
  readonly internalEdges: number;
  readonly boundaryEdges: number;
}

export type ContourSketch =
  | { readonly kind: 'none' }
  | { readonly kind: 'sealed' }
  | { readonly kind: 'lines'; readonly lines: number; readonly value: number };

function intervalLines(value: number): number {
  if (!Number.isFinite(value) || value <= 0) return 0;
  return Math.min(MAX_DRAWN_CONTOURS, Math.floor(value / CONTOUR_INTERVAL));
}

export function sketchContour(
  encoding: ContourEncodingId,
  sample: ContourSample,
): ContourSketch {
  if (sample.internalEdges <= 0) return { kind: 'none' };
  switch (encoding) {
    case 'edges_per_file': {
      const value = sample.files > 0 ? sample.internalEdges / sample.files : 0;
      return { kind: 'lines', lines: intervalLines(value), value };
    }
    case 'boundary_per_file': {
      const value = sample.files > 0 ? sample.boundaryEdges / sample.files : 0;
      return { kind: 'lines', lines: intervalLines(value), value };
    }
    case 'coupling_ratio': {
      if (sample.boundaryEdges <= 0) return { kind: 'sealed' };
      const value = sample.internalEdges / sample.boundaryEdges;
      return { kind: 'lines', lines: intervalLines(value), value };
    }
    default: {
      const unhandled: never = encoding;
      return unhandled;
    }
  }
}

export function contourCaption(encoding: ContourEncodingId, sample: ContourSample): string {
  const sketch = sketchContour(encoding, sample);
  switch (sketch.kind) {
    case 'none':
      return 'no relief';
    case 'sealed':
      return 'sealed';
    case 'lines':
      switch (encoding) {
        case 'coupling_ratio':
          return `${sketch.value.toFixed(2)} i/b`;
        case 'edges_per_file':
        case 'boundary_per_file':
          return `${sketch.value.toFixed(2)} /file`;
        default: {
          const unhandled: never = encoding;
          return unhandled;
        }
      }
    default: {
      const unhandled: never = sketch;
      return unhandled;
    }
  }
}

/** Equal file mass and equal internal edges; only the boundary differs. */
const EQUAL_MASS_LEAKY: ContourSample = { files: 40, internalEdges: 80, boundaryEdges: 80 };
const EQUAL_MASS_CLOSED: ContourSample = { files: 40, internalEdges: 80, boundaryEdges: 10 };
/** Same edges, four times the files: area grows, closure must not. */
const MASS_SMALL: ContourSample = { files: 10, internalEdges: 20, boundaryEdges: 10 };
const MASS_LARGE_SAME_EDGES: ContourSample = { files: 40, internalEdges: 20, boundaryEdges: 10 };
/** Internal edges with a measured-zero boundary. The ratio is unbounded. */
const SEALED: ContourSample = { files: 4, internalEdges: 8, boundaryEdges: 0 };

function sketchKey(sketch: ContourSketch): string {
  switch (sketch.kind) {
    case 'none':
      return 'none';
    case 'sealed':
      return 'sealed';
    case 'lines':
      return `lines:${sketch.lines}`;
    default: {
      const unhandled: never = sketch;
      return unhandled;
    }
  }
}

export function separatesEqualMassLeakage(encoding: ContourEncodingId): boolean {
  return (
    sketchKey(sketchContour(encoding, EQUAL_MASS_LEAKY)) !==
    sketchKey(sketchContour(encoding, EQUAL_MASS_CLOSED))
  );
}

export function independentOfFileMass(encoding: ContourEncodingId): boolean {
  return (
    sketchKey(sketchContour(encoding, MASS_SMALL)) ===
    sketchKey(sketchContour(encoding, MASS_LARGE_SAME_EDGES))
  );
}

export function statesSealedWithoutARatio(encoding: ContourEncodingId): boolean {
  return sketchContour(encoding, SEALED).kind === 'sealed';
}

/** The unique encoding that passes every witness, or null when the space is tied. */
export function winningContourEncoding(): ContourEncodingId | null {
  const winners = CONTOUR_ENCODINGS.filter(
    (encoding) =>
      separatesEqualMassLeakage(encoding) &&
      independentOfFileMass(encoding) &&
      statesSealedWithoutARatio(encoding),
  );
  return winners.length === 1 ? winners[0]! : null;
}
