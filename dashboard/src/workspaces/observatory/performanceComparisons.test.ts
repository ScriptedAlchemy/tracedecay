import { describe, expect, it } from 'vitest';
import {
  COMPARISON_DISPOSITIONS,
  COMPARISON_SUPPORT_FLOOR,
  UNPUBLISHED_COMPARISON_EVIDENCE,
  decideDisposition,
  dispositionPresentation,
  performanceComparisonBands,
  resultDimensions,
  subjectDimensions,
  type ComparisonEvidence,
} from './performanceComparisons.ts';

/**
 * The one rule this view exists to hold: `insufficient_evidence` is its own
 * disposition, never a softer spelling of `reject`.
 *
 * Every path that reaches it below is a path a naive implementation would have
 * reached `reject` on — no baseline, no lineage, thin support, no regression
 * finding — and each is asserted to be `insufficient_evidence` AND asserted not
 * to be `reject`, because the second assertion is the one that would fail if
 * someone later folded the states together.
 */

const COMPLETE: ComparisonEvidence = {
  baselineBuild: 'build-a1b2c3',
  candidateBuild: 'build-d4e5f6',
  lineageComplete: true,
  eligible: 120,
  pairedOutcomes: 118,
  regressionObserved: false,
};

describe('decideDisposition', () => {
  it('returns insufficient evidence — not reject — when no comparison is published', () => {
    const decision = decideDisposition(UNPUBLISHED_COMPARISON_EVIDENCE);
    expect(decision.disposition).toBe('insufficient_evidence');
    expect(decision.disposition).not.toBe('reject');
    expect(decision.reason).toContain('baseline and candidate build are not published');
  });

  it('returns insufficient evidence when only one side of the comparison is pinned', () => {
    expect(decideDisposition({ ...COMPLETE, candidateBuild: null }).disposition).toBe(
      'insufficient_evidence',
    );
    expect(decideDisposition({ ...COMPLETE, baselineBuild: null }).disposition).toBe(
      'insufficient_evidence',
    );
  });

  it('returns insufficient evidence when lineage is missing or unverified', () => {
    for (const lineage of [null, false]) {
      const decision = decideDisposition({ ...COMPLETE, lineageComplete: lineage });
      expect(decision.disposition).toBe('insufficient_evidence');
      expect(decision.disposition).not.toBe('reject');
    }
  });

  it('returns insufficient evidence — not reject — for under-floor support', () => {
    const decision = decideDisposition({
      ...COMPLETE,
      pairedOutcomes: COMPARISON_SUPPORT_FLOOR - 1,
    });
    expect(decision.disposition).toBe('insufficient_evidence');
    expect(decision.disposition).not.toBe('reject');
    expect(decision.reason).toContain(`${COMPARISON_SUPPORT_FLOOR}-outcome comparison floor`);
  });

  it('never reaches reject through an incomplete precondition, even with a regression present', () => {
    // The specific confusion the plan forbids: a regression observed under
    // evidence that could not be judged is not a rejection of the candidate.
    const incomplete: ComparisonEvidence[] = [
      { ...COMPLETE, baselineBuild: null, regressionObserved: true },
      { ...COMPLETE, lineageComplete: false, regressionObserved: true },
      { ...COMPLETE, pairedOutcomes: 2, regressionObserved: true },
      { ...COMPLETE, eligible: null, regressionObserved: true },
    ];
    for (const evidence of incomplete) {
      expect(decideDisposition(evidence).disposition).toBe('insufficient_evidence');
    }
  });

  it('reaches reject only from complete evidence over sufficient support', () => {
    const decision = decideDisposition({ ...COMPLETE, regressionObserved: true });
    expect(decision.disposition).toBe('reject');
  });

  it('reaches promote only from a reproducible accepted baseline with no regression', () => {
    expect(decideDisposition(COMPLETE).disposition).toBe('promote');
  });

  it('returns insufficient evidence when the results carry no regression finding at all', () => {
    // `null` is not `false`: an unrecorded finding cannot license a promotion.
    const decision = decideDisposition({ ...COMPLETE, regressionObserved: null });
    expect(decision.disposition).toBe('insufficient_evidence');
    expect(decision.disposition).not.toBe('promote');
  });

  it('always returns exactly one disposition from the closed set', () => {
    const cases: ComparisonEvidence[] = [
      UNPUBLISHED_COMPARISON_EVIDENCE,
      COMPLETE,
      { ...COMPLETE, regressionObserved: true },
      { ...COMPLETE, pairedOutcomes: 1 },
      { ...COMPLETE, lineageComplete: null },
    ];
    for (const evidence of cases) {
      const decision = decideDisposition(evidence);
      expect(COMPARISON_DISPOSITIONS).toContain(decision.disposition);
      expect(decision.reason.length).toBeGreaterThan(0);
    }
  });
});

describe('dispositionPresentation', () => {
  it('gives insufficient evidence its own state, distinct from reject', () => {
    const insufficient = dispositionPresentation('insufficient_evidence');
    const reject = dispositionPresentation('reject');
    expect(insufficient.state).toBe('unknown');
    expect(reject.state).toBe('denied');
    expect(insufficient.state).not.toBe(reject.state);
    expect(insufficient.label).not.toBe(reject.label);
  });

  it('says in words that insufficient evidence is not a rejection', () => {
    expect(dispositionPresentation('insufficient_evidence').meaning).toContain(
      'not a rejection',
    );
  });

  it('gives all three dispositions distinct labels and states', () => {
    const labels = COMPARISON_DISPOSITIONS.map(
      (disposition) => dispositionPresentation(disposition).label,
    );
    const states = COMPARISON_DISPOSITIONS.map(
      (disposition) => dispositionPresentation(disposition).state,
    );
    expect(new Set(labels).size).toBe(3);
    expect(new Set(states).size).toBe(3);
  });
});

describe('comparison evidence dimensions', () => {
  it('pins baseline and candidate builds as separate requirements', () => {
    const ids = subjectDimensions().map((dimension) => dimension.id);
    expect(ids).toContain('baseline_build');
    expect(ids).toContain('candidate_build');
    expect(ids).toContain('rollback_profile');
  });

  it('keeps per-stratum support, intervals, calibration, flakiness, and deviations separate', () => {
    const ids = resultDimensions().map((dimension) => dimension.id);
    expect(ids).toEqual([
      'outcome_counts',
      'stratum_support',
      'intervals',
      'calibration',
      'risk_coverage',
      'flaky_indeterminate',
      'deviations',
      'paired_outcomes',
    ]);
  });

  it('reports every comparison dimension as unpublished with the same stated reason', () => {
    for (const band of performanceComparisonBands()) {
      for (const dimension of band.dimensions) {
        expect(dimension.reading.kind).toBe('unpublished');
        expect(dimension.reading.kind === 'unpublished' && dimension.reading.reason).toContain(
          'no landed read route publishes a comparison record',
        );
      }
    }
  });

  it('does not claim a benchmark service or a separate database', () => {
    const reason = subjectDimensions()[0]!.reading;
    expect(reason.kind === 'unpublished' && reason.reason).toContain(
      'rather than a benchmark service or separate database',
    );
  });
});
