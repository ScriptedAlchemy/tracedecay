import type { EvidenceQuality } from '../../ui/EvidencePattern.tsx';
import type { ExplorerSourceProgressV1 } from '../../contracts/generated.ts';
import type { ExplorerStreamSnapshot } from './laneModel.ts';
import { LANES } from './model.ts';

/**
 * Whether Explorer has earned the right to say a term is absent from the index.
 *
 * A global-absence claim is the strongest statement this surface can make: it
 * tells the reader the thing does not exist anywhere, and a reader who believes
 * it stops looking. So it is derived from the coordinator's own numbers rather
 * than from any scalar that merely asserts it.
 *
 * The reason a blocked verdict carries prose is that the states it blocks on are
 * NOT interchangeable. "A source examined none of its 400 symbols" and "a source
 * could not determine the status of any of its 5 facts" are different facts
 * about the world, and collapsing them into one sentence about "incomplete
 * coverage" throws away the only information that tells a reader what to do
 * next.
 */
export type AbsenceVerdict =
  | { readonly confirmed: true; readonly quality: EvidenceQuality }
  | { readonly confirmed: false; readonly quality: EvidenceQuality; readonly reason: string };

/** Plural-aware unit naming, so a reason reads as prose rather than a template. */
function units(count: number, unit: string | null): string {
  const noun = unit ?? 'units';
  return `${count} ${count === 1 && noun.endsWith('s') ? noun.slice(0, -1) : noun}`;
}

/**
 * Why this source cannot contribute to a confirmed absence, or `null` if it can.
 *
 * The ordering is deliberate: a source that examined nothing is a more useful
 * thing to say than a source that left some units unaccounted for, so the
 * stronger statement is tested first.
 */
function coverageBlocker(source: ExplorerSourceProgressV1): string | null {
  const label = source.source_label;
  const { coverage } = source;
  const unit = coverage.unit;

  switch (source.outcome) {
    // `ready` earns absence through its coverage accounting below.
    case 'ready':
      break;
    case 'partial':
      return `${label} answered with omitted records`;
    case 'stale':
      return `${label} answered from a stale store`;
    case 'timed_out':
      return `${label} timed out before answering`;
    case 'unavailable':
    case 'error':
    case 'cancelled':
      return `${label} did not answer`;
    default: {
      const exhaustive: never = source.outcome;
      return exhaustive;
    }
  }
  if (coverage.completeness !== 'complete' || coverage.denominator === null) {
    return `${label} reports ${coverage.completeness} coverage`;
  }

  const denominator = coverage.denominator;
  // A `complete` claim is only checkable if the source said how it spent its
  // denominator. `DashboardCoverageV1::complete()` always fills these in, so a
  // Complete claim without them did not come from the canonical constructor and
  // has not shown its work.
  if (coverage.examined === null || coverage.unknown === null || coverage.omitted === null) {
    return `${label} claims complete coverage without accounting for its ${units(
      denominator,
      unit,
    )}`;
  }
  // Nothing to examine is legitimately complete; refusing this would make an
  // empty index permanently unable to report itself as empty.
  if (denominator === 0) return null;

  if (coverage.examined === 0) {
    return `${label} examined none of its ${units(denominator, unit)}`;
  }
  if (coverage.unknown >= denominator) {
    return `${label} could not determine the status of any of its ${units(denominator, unit)}`;
  }
  const unaccounted = coverage.unknown + coverage.omitted;
  if (unaccounted > 0) {
    const parts: string[] = [];
    if (coverage.unknown > 0) parts.push(`${coverage.unknown} unknown`);
    if (coverage.omitted > 0) parts.push(`${coverage.omitted} omitted`);
    return `${label} left ${parts.join(' and ')} of its ${units(denominator, unit)}`;
  }
  return null;
}

/**
 * The verdict for one submitted stream.
 *
 * A terminal frame alone cannot confirm absence: each source must report
 * complete coverage and an accounted denominator.
 */
export function absenceVerdict(run: ExplorerStreamSnapshot | undefined): AbsenceVerdict {
  if (run === undefined) {
    return {
      confirmed: false,
      quality: 'unknown',
      reason: 'no stream has answered for this query',
    };
  }
  if (run.sources.length !== LANES.length) {
    return {
      confirmed: false,
      quality: 'unknown',
      reason: 'not all required sources have answered',
    };
  }
  for (const source of run.sources) {
    const blocker = coverageBlocker(source);
    if (blocker !== null) return { confirmed: false, quality: 'unknown', reason: blocker };
  }
  if (run.state !== 'completed') {
    return {
      confirmed: false,
      quality: 'unknown',
      reason: 'the stream has not completed with complete coverage',
    };
  }
  return { confirmed: true, quality: 'measured' };
}
