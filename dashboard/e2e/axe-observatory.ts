/**
 * Plan 26 canonical observations — Observatory.
 *
 * Own module rather than more of `axe-audit.ts`, for the reason
 * `axe-workspaces.ts` gives: `withoutValue` is a payload builder nothing else
 * uses, because Observatory is the only route where a whole producing source
 * can go missing while its siblings answer. The plate readings these assert
 * with are shared with Costs and live in `axe-measurements.ts`; the envelope
 * they are edited into is shared with Costs and Code and lives in
 * `axe-envelopes.ts`.
 *
 * The four states are chosen for what they put next to each other: a measured
 * zero beside a missing measurement, a failed source beside answering ones, a
 * complete read that is separately redacted, and a read model carrying no
 * measurements at all.
 */
import {
  expectAbsent,
  expectContains,
  expectEqual,
  expectVisibleText,
  type Scenario,
} from './axe-harness.ts';
import { envelopeFixture } from './axe-envelopes.ts';
import {
  assertMeasurementIsSelfDescribing,
  assertMetricPlateTruth,
  plateReading,
} from './axe-measurements.ts';

const OBSERVATORY = '/api/observatory';

/**
 * One measurement turned into the projector's own unavailable reading.
 *
 * Derived from the fixture's metric so descriptor, provenance and cohort stay
 * the shapes the contract gate already checks, and it nulls the denominator
 * size and drops coverage to `unknown` exactly as
 * `application::observability` does — a plate that kept an eligible count
 * beside a missing value would be a state the projector never emits.
 */
function withoutValue(metric: Record<string, unknown>, reason: string): Record<string, unknown> {
  const coverage = metric['coverage'] as Record<string, unknown>;
  return {
    ...metric,
    value: null,
    denominator_value: null,
    unavailable_reason: reason,
    coverage: {
      ...coverage,
      state: 'unknown',
      eligible: null,
      observed: 0,
      completed: 0,
      unknown: 1,
    },
    uncertainty: { lower: null, upper: null, reason },
  };
}

export const OBSERVATORY_SCENARIOS: readonly Scenario[] = [
  {
    id: 'observatory-canonical',
    route: '/observatory',
    proves:
      'a measured zero and a missing measurement sit side by side on the same panel and stay distinguishable',
    overrides: {},
    assert: async (page) => {
      await assertMetricPlateTruth(page, 'the canonical observations panel');
      // A rate the projector measured as exactly zero. It is a reading, so it
      // prints as one — the em dash belongs to the plate three tiles over,
      // whose value does not exist.
      expectEqual(
        (await plateReading(page, 'feedback_denial_rate')).figure,
        '0',
        'a measured zero rate',
      );
      expectEqual(
        (await plateReading(page, 'feedback_denial_rate')).available,
        'true',
        'the measured zero is marked available',
      );
      const missing = await plateReading(page, 'feedback_revocation_propagation_p95');
      expectEqual(missing.figure, '—', 'a measurement that does not exist');
      expectEqual(missing.available, 'false', 'the missing measurement is marked unavailable');
      await expectVisibleText(page, 'no_revocation_observations', "the projector's own reason");
      // The requirement the plate exists for: the figure, the unit that scales
      // it and the population it is over must all be announced together.
      await assertMeasurementIsSelfDescribing(page, 'feedback_coverage', {
        figure: '91.27',
        unit: '%',
        denominator: 'per eligible observations · 1,884',
      });
      await assertMeasurementIsSelfDescribing(page, 'feedback_latency_p95', {
        figure: '214.8',
        unit: 'ms',
        denominator: 'per latency samples · 1,884',
      });
      await expectVisibleText(page, 'incomplete_metric_coverage', 'the omission reason, verbatim');
    },
  },
  {
    id: 'observatory-source-unreadable',
    route: '/observatory',
    proves:
      'a whole producing source that returned no measurement reads as zero-of-three measured, never as three zeroes',
    overrides: {
      [OBSERVATORY]: {
        status: 200,
        body: envelopeFixture(OBSERVATORY, (envelope, payload) => {
          const metrics = payload['metrics'] as Record<string, unknown>[];
          payload['metrics'] = metrics.map((metric) =>
            (metric['provenance'] as Record<string, unknown>)['source'] === 'observability_envelope'
              ? withoutValue(metric, 'the observability envelope store could not be opened')
              : metric,
          );
          envelope['domain_state'] = 'partial';
        }),
      },
    },
    assert: async (page) => {
      await assertMetricPlateTruth(page, 'an unreadable producing source');
      const tally = await page.evaluate(() => {
        const group = document.querySelector('[data-metric-source="observability_envelope"]');
        return Array.from(group?.querySelectorAll('span') ?? [])
          .map((span) => (span.textContent ?? '').trim())
          .find((text) => /^\d+ of \d+ measured$/.test(text));
      });
      expectEqual(tally, '0 of 3 measured', 'the unreadable source tally');
      await expectVisibleText(
        page,
        'the observability envelope store could not be opened',
        'the failure reason on every plate of the failed source',
      );
    },
  },
  {
    id: 'observatory-redacted',
    route: '/observatory',
    proves:
      'THE AUTHORIZATION AXIS — a read that is complete and separately redacted shows both, not one collapsed into the other',
    overrides: {
      [OBSERVATORY]: {
        status: 200,
        body: envelopeFixture(OBSERVATORY, (envelope) => {
          envelope['authorization'] = { outcome: 'redacted' };
          // Deliberately `ready`: the domain state and the authorization
          // outcome are independent axes, and the bug this guards against is a
          // surface that shows one chip and drops whichever axis it did not
          // pick.
          envelope['domain_state'] = 'ready';
        }),
      },
    },
    assert: async (page) => {
      const states = await page.evaluate(() =>
        Array.from(document.querySelectorAll('[data-state]')).map((chip) => ({
          state: chip.getAttribute('data-state') ?? '',
          text: (chip.textContent ?? '').replace(/\s+/g, ' ').trim(),
        })),
      );
      const redacted = states.find((chip) => chip.state === 'redacted');
      if (redacted === undefined) {
        throw new Error(
          `FALSIFIED: a redacted read rendered no redacted chip. Chips on the page: ${JSON.stringify(states.map((chip) => chip.state))}`,
        );
      }
      expectContains(redacted.text, 'read authorization', 'the redacted chip names its axis');
      if (!states.some((chip) => chip.state === 'ready')) {
        throw new Error(
          'FALSIFIED: the redaction replaced the domain state instead of sitting beside it',
        );
      }
      // Redaction is not a reason to stop reporting what WAS returned.
      await assertMetricPlateTruth(page, 'a redacted read');
    },
  },
  {
    id: 'observatory-no-metrics',
    route: '/observatory',
    proves: 'a read model carrying no measurements says so, and does not render a panel of zeroes',
    overrides: {
      [OBSERVATORY]: {
        status: 200,
        body: envelopeFixture(OBSERVATORY, (envelope, payload) => {
          payload['metrics'] = [];
          envelope['domain_state'] = 'complete_zero_findings';
        }),
      },
    },
    assert: async (page) => {
      await expectVisibleText(
        page,
        'this is a payload with no metrics, not a set of zeroes',
        'the empty read-model sentence',
      );
      await expectAbsent(page, '[data-metric]', 'no metric plates behind an empty read model');
    },
  },
];
