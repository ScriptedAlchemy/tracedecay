/**
 * The evidence-grade ladder, applied to what Explorer shows.
 *
 * Every identity, count and quoted text on the surface carries exactly one
 * grade from the shared ladder, and a source class beside it saying where the
 * record was persisted. Grade describes the support behind a claim; it is
 * never a confidence figure, and no visual treatment upgrades one.
 */
import type { ExplorerLaneReadModel } from './laneModel.ts';
import type { Hit, SourceLaneId } from './model.ts';

export type EvidenceGrade =
  | 'EXACT'
  | 'EXPLICIT'
  | 'INFERRED'
  | 'AMBIGUOUS'
  | 'STALE'
  | 'UNAVAILABLE';

/** Where a record was persisted or observed. Orthogonal to grade. */
export type SourceClass = 'GRAPH' | 'TRANSCRIPT' | 'FACT';

export const SOURCE_CLASS: Record<SourceLaneId, SourceClass> = {
  code: 'GRAPH',
  sessions: 'TRANSCRIPT',
  knowledge: 'FACT',
};

export interface HitEvidence {
  readonly sourceClass: SourceClass;
  /** The grade of the row's identity: which record this is. */
  readonly identity: EvidenceGrade;
  /** The grade of the row's quoted text: what the record says. */
  readonly text: EvidenceGrade;
  /** The basis, in one sentence, so a reader can check the grade. */
  readonly basis: string;
}

/**
 * A row's grades.
 *
 * Identity is `EXACT` for every lane: each row is a record the owning
 * authority returned under its own stable key. Text differs. A code row's
 * signature and path are source facts read from the index, so they stay
 * `EXACT`. A transcript message or a memory fact is persisted language, what
 * a person or agent wrote, or what the curator recorded, and is presented as
 * a claim, `EXPLICIT`, not as repository truth.
 */
export function hitEvidence(hit: Hit): HitEvidence {
  switch (hit.lane) {
    case 'code':
      return {
        sourceClass: 'GRAPH',
        identity: 'EXACT',
        text: 'EXACT',
        basis: `symbol ${hit.titleField} and location read from the code graph under its stable node id`,
      };
    case 'sessions':
      return {
        sourceClass: 'TRANSCRIPT',
        identity: 'EXACT',
        text: 'EXPLICIT',
        basis: `persisted transcript record from the LCM store; ${hit.titleField} is language a participant wrote, quoted as a claim`,
      };
    case 'knowledge':
      return {
        sourceClass: 'FACT',
        identity: 'EXACT',
        text: 'EXPLICIT',
        basis: `fact record from the bounded memory store; ${hit.titleField} is the curated statement, quoted as a claim`,
      };
    default: {
      const exhaustive: never = hit.lane;
      return exhaustive;
    }
  }
}

/**
 * The grade a lane's rows are served at, or `null` while no claim exists yet.
 *
 * `ready` and `partial` rows are records from their owning authority, so the
 * lane grades `EXACT`; partial states its omission separately. A `stale`
 * source exists outside its freshness window and grades `STALE` rather than
 * being served as current. Everything that did not answer is `UNAVAILABLE`,
 * and a lane still reading makes no claim at all.
 */
export function laneGrade(read: ExplorerLaneReadModel): EvidenceGrade | null {
  switch (read.state) {
    case 'pending':
      return null;
    case 'ready':
    case 'partial':
      return 'EXACT';
    case 'stale':
      return 'STALE';
    case 'timed_out':
    case 'unavailable':
    case 'cancelled':
    case 'error':
    case 'offline':
    case 'unauthorized':
    case 'denied':
    case 'locked':
    case 'unsupported_schema':
    case 'unanswered':
    case 'unregistered':
    case 'indeterminate':
      return 'UNAVAILABLE';
    default: {
      const exhaustive: never = read;
      return exhaustive;
    }
  }
}
