/**
 * Plan 26 canonical cost observations, and the latency panel that has no
 * measurement behind it — Costs.
 *
 * Own module rather than more of `axe-audit.ts` because the route is two reads
 * behind one page, and its scenarios are about which of them is allowed to take
 * the other down with it: an unpriced ledger, a projection that failed while
 * the savings read above it answered, and an authorization outcome that is
 * neither. The plate readings are shared with Observatory
 * (`axe-measurements.ts`) and so is the envelope they are edited into
 * (`axe-envelopes.ts`); what is left here is only what /costs claims.
 */
import {
  expectAbsent,
  expectContains,
  expectEqual,
  expectVisibleText,
  type Scenario,
} from './axe-harness.ts';
import { envelopeFixture } from './axe-envelopes.ts';
import { assertMetricPlateTruth, plateReading } from './axe-measurements.ts';

const COSTS = '/api/costs';

export const COSTS_SCENARIOS: readonly Scenario[] = [
  {
    id: 'costs-canonical',
    route: '/costs',
    proves:
      'THE UNSUPPORTED STATE — an unpriced cost and an unmeasured latency each print a reason where a figure would go',
    overrides: {},
    assert: async (page) => {
      await assertMetricPlateTruth(page, 'the canonical cost panel');
      // Prices are recorded at ingest. Turns counted without a pricing
      // revision have no cost — which is an accounting state, not $0.00.
      const cost = await plateReading(page, 'provider_cost');
      expectEqual(cost.figure, '—', 'an unpriced cost');
      expectEqual(cost.available, 'false', 'the unpriced cost is marked unavailable');
      if (cost.figure.includes('0')) {
        throw new Error('FALSIFIED: an unpriced turn ledger rendered as a zero bill');
      }
      await expectVisibleText(page, 'pricing_revision_unavailable', 'the pricing failure reason');
      await expectVisibleText(
        page,
        'none attached to this read',
        'the missing pricing revision, stated rather than dashed',
      );
      // The latency panel has no measurement anywhere behind it, so it carries
      // the `unsupported` state and says where the one real latency lives.
      const latency = page.locator('[data-costs-latency]');
      expectEqual(
        (await latency.getAttribute('data-costs-latency')) ?? '',
        'unavailable',
        'the latency panel state',
      );
      const chip = await latency.locator('[data-state]').first().getAttribute('data-state');
      expectEqual(chip ?? '', 'unsupported', 'the latency panel chip');
      await expectVisibleText(
        page,
        'no provider latency is measured',
        'the unsupported latency reason',
      );
      await expectAbsent(
        page,
        '[data-costs-latency] [data-cell="numeric"]',
        'no figure inside a panel with nothing to measure',
      );
    },
  },
  {
    id: 'costs-read-failed',
    route: '/costs',
    proves:
      'THE SPLIT READ — the canonical projection failing leaves the savings ledger above it fully rendered',
    overrides: { [COSTS]: { status: 500, body: { detail: 'costs projector unavailable' } } },
    assert: async (page) => {
      await expectVisibleText(page, 'HTTP 500', 'the transport failure, named');
      await expectAbsent(page, '[data-metric]', 'no cost plates behind a failed read');
      // The whole point of two boundaries: the other read still answered. Read
      // through visible text only — `ReadoutBar`'s `label` becomes an
      // `aria-label` on a plain div rather than anything on screen.
      await expectVisibleText(page, 'turn ledger', 'the savings ledger survived the failure');
      await expectVisibleText(page, 'total cost', 'the spend readout survived too');
      await expectVisibleText(page, 'Where the tokens go', 'and so did the token mix');
    },
  },
  {
    id: 'costs-denied',
    route: '/costs',
    proves: 'a denied authorization is its own axis on Costs, beside whatever the read itself was',
    overrides: {
      [COSTS]: {
        status: 200,
        body: envelopeFixture(COSTS, (envelope) => {
          envelope['authorization'] = { outcome: 'denied' };
        }),
      },
    },
    assert: async (page) => {
      const denied = page.locator('[data-state="denied"]').first();
      if ((await denied.count()) === 0) {
        throw new Error('FALSIFIED: a denied read rendered no denied chip');
      }
      expectContains(
        (await denied.textContent()) ?? '',
        'read authorization',
        'the denied chip names its axis',
      );
    },
  },
];
