/**
 * The Knowledge workspace's measured channels, as pure functions.
 *
 * Three separate readings on this page were uninformative or simply wrong, and
 * all three are distribution problems rather than layout problems:
 *
 *   1. The trust-distribution plate rendered ten empty bars. It is fed by
 *      `overview.trust_histogram`, and on a real store every bucket in that
 *      array is zero — `dashboard_compatibility_named_counts_tx` emits the
 *      bucket name as `"trust-9"` while `facts.rs::trust_histogram` reads it
 *      with `row.name.parse::<usize>()`, which fails and skips the row. The
 *      distribution genuinely exists (the memory status route reports 161
 *      facts at 0.75-1.00 and 21 at 0.50-0.75) so the view takes the first
 *      source that carries any mass and names which one it used.
 *
 *   2. Every visible fact reads trust 1.00 — not because trust is uniform, but
 *      because the fact list is a top-100 slice ordered so the high-trust
 *      facts fill it. The slice's own spread is stated instead of drawn, and
 *      the part of the store the slice cannot reach is stated with it.
 *
 *   3. HRR coverage was six bars all at or near 100%, which is six drawings of
 *      one fact. The uniformity is stated once and only the banks that deviate
 *      are drawn.
 */
import {
  assertNever,
  type MemoryFactRowV1,
  type MemoryHrrCoverageV1,
} from '../../contracts/generated.ts';

export interface TrustBand {
  /** Printed label, e.g. `0.75–1.00`. */
  label: string;
  count: number;
  /** Inclusive lower edge, 0–1. */
  lower: number;
  /** Exclusive upper edge (inclusive at 1). */
  upper: number;
}

export type TrustSource = 'histogram' | 'status_bands' | 'loaded_facts' | 'none';

export interface TrustDistribution {
  source: TrustSource;
  bands: TrustBand[];
  total: number;
  /** How many bands actually hold facts. */
  occupied: number;
  /**
   * Every fact is in one band, so there is no shape to draw. The view states
   * the reading instead of rendering one full bar beside a row of empty ones.
   */
  degenerate: boolean;
}

const EMPTY: TrustDistribution = {
  source: 'none',
  bands: [],
  total: 0,
  occupied: 0,
  degenerate: false,
};

/** The four bands `memory_api.rs::status` reports, which is the only trust
 * distribution a real store currently serves correctly. */
export interface TrustStatusBands {
  trust_0_025_count?: number | undefined;
  trust_025_050_count?: number | undefined;
  trust_050_075_count?: number | undefined;
  trust_075_100_count?: number | undefined;
}

function finish(source: TrustSource, bands: TrustBand[]): TrustDistribution {
  const total = bands.reduce((sum, band) => sum + band.count, 0);
  if (total === 0) return EMPTY;
  const occupied = bands.filter((band) => band.count > 0).length;
  return { source, bands, total, occupied, degenerate: occupied <= 1 };
}

/**
 * Pick the finest-grained trust distribution that actually carries mass.
 *
 * Order is deliberate. The ten-bucket histogram is the most informative when
 * it works, so it is tried first; the four status bands are coarser but count
 * the WHOLE store; the loaded facts are finest of all but only describe the
 * slice that was fetched. Falling through to a source that says less about
 * more is better than drawing a plate of zeroes, and the view prints which
 * source it landed on so the reader knows what the counts cover.
 */
export function composeTrustDistribution(
  histogram: ReadonlyArray<{ label: string; count: number; bucket: number }> | undefined,
  status: TrustStatusBands | undefined,
  facts: ReadonlyArray<Pick<MemoryFactRowV1, 'trust_score'>> | undefined,
): TrustDistribution {
  const fromHistogram = (histogram ?? []).map((bucket) => ({
    label: bucket.label,
    count: bucket.count,
    lower: bucket.bucket / 10,
    upper: (bucket.bucket + 1) / 10,
  }));
  const histogramResult = finish('histogram', fromHistogram);
  if (histogramResult.total > 0) return histogramResult;

  const fromStatus: TrustBand[] = [
    { label: '0.00–0.25', lower: 0, upper: 0.25, count: status?.trust_0_025_count ?? 0 },
    { label: '0.25–0.50', lower: 0.25, upper: 0.5, count: status?.trust_025_050_count ?? 0 },
    { label: '0.50–0.75', lower: 0.5, upper: 0.75, count: status?.trust_050_075_count ?? 0 },
    { label: '0.75–1.00', lower: 0.75, upper: 1, count: status?.trust_075_100_count ?? 0 },
  ];
  const statusResult = finish('status_bands', fromStatus);
  if (statusResult.total > 0) return statusResult;

  const counts = new Array(10).fill(0) as number[];
  for (const fact of facts ?? []) {
    const index = Math.min(9, Math.max(0, Math.floor(fact.trust_score * 10)));
    counts[index] = (counts[index] ?? 0) + 1;
  }
  return finish(
    'loaded_facts',
    counts.map((count, index) => ({
      label: `${(index / 10).toFixed(1)}–${((index + 1) / 10).toFixed(1)}`,
      count,
      lower: index / 10,
      upper: (index + 1) / 10,
    })),
  );
}

/** What the counts in a distribution actually cover, for the plate's caption.
 * Never guessed — each source has one true answer. */
export function trustSourceNote(source: TrustSource): string {
  switch (source) {
    case 'histogram':
      return 'every fact in the store';
    case 'status_bands':
      return 'every fact in the store, in the four bands the status route serves';
    case 'loaded_facts':
      return 'only the facts loaded below — the store reported no distribution';
    case 'none':
      return 'no source reported a distribution';
    default:
      return assertNever(source);
  }
}

export interface LoadedTrust {
  count: number;
  min: number;
  max: number;
  /** How many of the loaded facts sit at exactly the maximum. */
  atMax: number;
  /** `max - min`. */
  spread: number;
  /**
   * True when the loaded facts are packed tightly enough that a per-row rail
   * scaled 0–1 is the same length on every row. Below this the rail is not a
   * ranking, it is decoration on a column of identical values.
   */
  flat: boolean;
}

/** The trust spread across the facts actually on screen. `null` when there is
 * nothing loaded to measure. */
export function summarizeLoadedTrust(
  facts: ReadonlyArray<Pick<MemoryFactRowV1, 'trust_score'>>,
  flatThreshold = 0.25,
): LoadedTrust | null {
  const scores = facts
    .map((fact) => fact.trust_score)
    .filter((score): score is number => typeof score === 'number' && Number.isFinite(score));
  if (scores.length === 0) return null;
  const min = Math.min(...scores);
  const max = Math.max(...scores);
  return {
    count: scores.length,
    min,
    max,
    atMax: scores.filter((score) => score === max).length,
    spread: max - min,
    flat: max - min < flatThreshold,
  };
}

/**
 * How many facts the whole store holds below the loaded slice's floor.
 *
 * The slice is ordered so the highest-trust facts fill it; without this the
 * reader would conclude the store has no low-trust facts, which is exactly the
 * wrong conclusion. Null when the bands cannot answer (the floor falls inside
 * a band rather than on its edge, so any number would be an interpolation).
 */
export function factsBelow(
  distribution: TrustDistribution,
  floor: number,
): number | null {
  if (distribution.total === 0) return null;
  const straddled = distribution.bands.find(
    (band) => floor > band.lower && floor < band.upper && band.count > 0,
  );
  if (straddled) return null;
  return distribution.bands
    .filter((band) => band.upper <= floor)
    .reduce((sum, band) => sum + band.count, 0);
}

export interface HrrSummary {
  categories: number;
  /** Lowest per-category coverage, 0–1. */
  floor: number;
  /** Banks whose status is anything other than `ready`. */
  exceptions: MemoryHrrCoverageV1[];
  /** The one-line reading that replaces a rail of near-identical bars. */
  line: string;
}

/**
 * HRR coverage as one sentence plus its exceptions.
 *
 * Six categories at 96–100% drawn as six bars is one fact rendered six times,
 * and it buries the reading that does vary: four of those banks are stale or
 * incompletely vectorized, which no coverage percentage shows.
 */
export function summarizeHrrCoverage(rows: readonly MemoryHrrCoverageV1[]): HrrSummary | null {
  if (rows.length === 0) return null;
  const floor = rows.reduce((min, row) => Math.min(min, row.coverage), 1);
  const exceptions = rows.filter((row) => row.status !== 'ready');
  const uniform = 1 - floor < 0.1;
  const coverageLine = uniform
    ? `All ${rows.length} categories are at least ${Math.floor(floor * 100)}% vectorized.`
    : `Coverage runs from ${Math.floor(floor * 100)}% to ${Math.round(
        rows.reduce((max, row) => Math.max(max, row.coverage), 0) * 100,
      )}% across ${rows.length} categories.`;
  const line =
    exceptions.length === 0
      ? `${coverageLine} Every bank is ready.`
      : `${coverageLine} ${exceptions.length} of ${rows.length} banks are not ready:`;
  return { categories: rows.length, floor, exceptions, line };
}

/** Human wording for a coverage status, so the row does not print an
 * identifier at a reader.
 *
 * The contract types this as an open string, and it is: `memory_api.rs` is free
 * to grow a fifth status. An unrecognised one is shown verbatim rather than
 * mapped onto the nearest known wording — the reader learns the store said
 * something this build does not know about, which is the truth, instead of
 * being told a state the daemon never reported. */
export function hrrStatusLabel(status: MemoryHrrCoverageV1['status']): string {
  switch (status) {
    case 'ready':
      return 'ready';
    case 'missing_bank':
      return 'no bank';
    case 'missing_vectors':
      return 'missing vectors';
    case 'stale_bank':
      return 'stale bank';
    default:
      return status;
  }
}
