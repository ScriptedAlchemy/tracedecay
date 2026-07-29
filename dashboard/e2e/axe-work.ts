/**
 * The thirteenth channel, whose data plane is deliberately closed — Work.
 *
 * Work is routed and navigable and has no generated read model behind it, so
 * the only claim it makes is about its own contract inventory. That makes it
 * the easiest surface in the app to get wrong in the direction this whole
 * gate exists to catch: an uncontracted workspace that quietly starts drawing
 * lanes, figures and controls it never read. No override is needed — the
 * surface issues no request at all, which is itself part of the claim.
 *
 * Own module rather than more of `axe-audit.ts` for that last reason: it is the
 * one surface whose evidence is entirely negative, so it has no payload builder
 * and never will, and the assertion is a per-row inventory no other route
 * performs.
 */
import { expectAbsent, expectEqual, expectVisibleText, type Scenario } from './axe-harness.ts';

export const WORK_SCENARIOS: readonly Scenario[] = [
  {
    id: 'work-contract-gate',
    route: '/work',
    proves:
      'the thirteenth channel states its withheld authority per surface, and draws no figure, lane or command it has no contract for',
    overrides: {},
    // A dense ruled ledger is exactly the shape that traps content in a
    // collapsed scroller at 400% zoom, and this is the newest surface in the
    // app, so it carries the matrix for /work.
    matrix: true,
    assert: async (page) => {
      expectEqual(
        (await page.locator('[data-work-authority]').getAttribute('data-work-authority')) ?? '',
        'uncontracted',
        'the Work authority reading',
      );
      await expectVisibleText(
        page,
        'No generated Work read model is available in this build.',
        'the contract-gate sentence',
      );
      // Per row, not per page: a ledger that lost one row's state would still
      // pass a page-level check for "some unsupported chip is present".
      const rows = await page.evaluate(() =>
        Array.from(document.querySelectorAll('[data-work-surface]')).map((row) => ({
          id: row.getAttribute('data-work-surface') ?? '',
          state: row.querySelector('[data-state]')?.getAttribute('data-state') ?? '',
          requires: (row.querySelector('td .td-value')?.textContent ?? '').trim(),
        })),
      );
      if (rows.length === 0) throw new Error('Work rendered no withheld surfaces at all');
      const unstated = rows.filter(
        (row) => row.state !== 'unsupported' && row.state !== 'unsupported_schema',
      );
      if (unstated.length > 0) {
        throw new Error(
          `FALSIFIED: a withheld Work surface carries an available state: ${JSON.stringify(unstated)}`,
        );
      }
      const nameless = rows.filter((row) => row.requires === '');
      if (nameless.length > 0) {
        throw new Error(
          `a withheld surface names no contract, so the gap is unactionable: ${JSON.stringify(nameless)}`,
        );
      }
      // The defect this route is most exposed to. The header's channel number
      // is a real reading; below it there is nothing measured, nothing to
      // command, and nowhere to deep-link into.
      await expectAbsent(
        page,
        '[data-work-ledger] [data-cell="numeric"]',
        'no figure on a surface with nothing to measure',
      );
      await expectAbsent(
        page,
        '[data-work-ledger] button',
        'no command offered without a command contract',
      );
      await expectAbsent(
        page,
        '[data-work-ledger] a[href]',
        'no deep link into a projection that does not exist',
      );
    },
  },
];
