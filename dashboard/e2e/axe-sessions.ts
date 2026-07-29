/**
 * The transcript drill-down — Sessions. The inspector column is
 * `max-md:hidden`, so these scan the drill-down at 768 and 1440 and the bare
 * list at 320.
 *
 * Own module rather than more of `axe-audit.ts`, for the reason
 * `axe-workspaces.ts` gives: these three scenarios need payload builders
 * nothing else uses. `transcriptPages` is the largest of them and the least
 * reusable — it is a server, not a fixture — and the two keyboard helpers
 * beside it exist only to reach the state the pager assertion is about.
 *
 * `openTranscript` is the one export that faces outward. `sessions-canary`
 * plants its markup in the drill-down rather than in the list behind it, so the
 * canary and these scenarios have to open the surface the same way; it lives
 * here, with the route knowledge it encodes, and `axe-canary.ts` imports it.
 */
import type { Page } from '@playwright/test';
import { resolveFixture } from '../stories/fixtures/data.ts';
import {
  expectAbsent,
  expectContains,
  expectEqual,
  expectVisibleText,
  focusedElement,
  openRow,
  type Scenario,
} from './axe-harness.ts';

/** The transcript drill-down. The trailing slash keeps the override off the
 * sibling `/sessions` list route, which is a different payload entirely. */
const LCM_SESSION = '/api/plugins/hermes-lcm/session/';

/**
 * A transcript served as REAL server pages: `limit`, `offset` and `order` are
 * honoured, and `has_more_messages` turns over on the last page.
 *
 * A fixed body would answer `offset=200` with page one, so the pager would
 * appear to work while nothing moved — and the focus assertion below would be
 * measuring a frozen fixture rather than the surface's behaviour when the
 * control it was activated from disables itself.
 */
function transcriptPages(total: number): (url: URL) => Record<string, unknown> {
  const base = structuredClone(resolveFixture(LCM_SESSION, '')) as Record<string, unknown>;
  const template = (base['messages'] as Record<string, unknown>[])[0]!;
  const counts = base['counts'] as Record<string, unknown>;
  return (url) => {
    const offset = Number(url.searchParams.get('offset') ?? '0');
    const limit = Number(url.searchParams.get('limit') ?? '100');
    const served = Math.max(0, Math.min(limit, total - offset));
    return {
      ...base,
      order: url.searchParams.get('order') ?? 'asc',
      limit,
      offset,
      has_more: offset + served < total,
      has_more_messages: offset + served < total,
      counts: { ...counts, message_count: total },
      messages: Array.from({ length: served }, (_, i) => ({
        ...template,
        message_id: `page-${offset}-${i}`,
        ordinal: offset + i,
        role: (offset + i) % 3 === 0 ? 'user' : 'assistant',
        content: `turn ${offset + i + 1} of ${total}`,
      })),
    };
  };
}

/** Open the first session in the list, which is what mounts the drill-down. */
export function openTranscript(page: Page): Promise<void> {
  return openRow(page, /-2026-07-/);
}

/**
 * Page forward with the keyboard, if this viewport shows the pager.
 *
 * Keyboard rather than mouse because that is the population the assertion is
 * about: a mouse click leaves focus where the pointer put it, which would hide
 * the very thing being measured. Tolerant of a missing pager on purpose — the
 * inspector column is `max-md:hidden`, so at 320 there is nothing to page, and
 * that is a layout fact rather than an accessibility finding. The strict
 * version of this runs in the assertion, at 1440.
 */
async function pageForward(page: Page, settledOn: RegExp): Promise<void> {
  const next = page.getByRole('button', { name: 'Next page' });
  if ((await next.count()) === 0) return;
  await next.first().focus();
  await page.keyboard.press('Enter');
  await page
    .getByText(settledOn)
    .first()
    .waitFor({ timeout: 15_000 })
    .catch(() => {
      /* asserted at 1440; a narrow layout that never advanced is not a finding */
    });
}

export const SESSIONS_SCENARIOS: readonly Scenario[] = [
  {
    id: 'sessions-transcript',
    route: '/sessions',
    proves:
      'the transcript drill-down and its compaction boundaries are scannable, and both scrolling lists are reachable by keyboard and named',
    overrides: {},
    drive: openTranscript,
    assert: async (page) => {
      await expectVisibleText(page, 'compaction boundaries', 'the compaction section');
      expectEqual(
        String(await page.locator('[data-summary-node]').count()),
        '3',
        'the compaction boundary rows',
      );
      await expectVisibleText(page, 'Summaries hold', 'the derived compaction ratio');
      await expectVisibleText(page, 'raw messages', 'the transcript section');
      // A scrollable list of read-out rows has nothing inside it to tab to, so
      // the list itself must take the tab stop (WCAG 2.1.1) — and a tab stop
      // that announces nothing is its own problem, which no axe rule reports.
      const lists = await page.evaluate(() =>
        Array.from(document.querySelectorAll('ol[tabindex]')).map((list) => ({
          label: list.getAttribute('aria-label') ?? '',
          tabindex: list.getAttribute('tabindex') ?? '',
        })),
      );
      if (lists.length < 2) {
        throw new Error(
          `expected the transcript and the boundary list to both take a tab stop, found ${lists.length}`,
        );
      }
      for (const list of lists) {
        expectEqual(list.tabindex, '0', 'a transcript list tab stop');
        if (list.label === '') throw new Error('a focusable transcript list announces no name');
      }
    },
  },
  {
    id: 'sessions-transcript-paged',
    route: '/sessions',
    proves:
      'THE PAGER — reaching the last page with the keyboard does not drop focus to the document when Next disables itself',
    overrides: { [LCM_SESSION]: { status: 200, bodyFor: transcriptPages(250) } },
    drive: async (page) => {
      await openTranscript(page);
      await pageForward(page, /101–200 of 250/);
      await pageForward(page, /201–250 of 250/);
    },
    assert: async (page) => {
      // The read really advanced: this is a server page, not the first page
      // relabelled.
      await expectVisibleText(page, '201–250 of 250', 'the last page range');
      await expectVisibleText(page, 'last page', 'the last-page marker');
      await expectVisibleText(page, 'turn 250 of 250', 'the last turn of the last page');
      const next = page.getByRole('button', { name: 'Next page' });
      expectEqual(String(await next.isDisabled()), 'true', 'Next is disabled on the last page');
      expectEqual(
        String(await page.getByRole('button', { name: 'Previous page' }).isDisabled()),
        'false',
        'Previous is available on the last page',
      );
      // The defect this scenario exists for. Activating Next on the second-to-
      // last page disables the control that was activated, and a keyboard user
      // is silently returned to the top of the document.
      const focused = await focusedElement(page);
      if (focused === 'body') {
        throw new Error(
          'FALSIFIED: paging to the last page disabled the focused control and dropped focus to the document, ' +
            'so a keyboard user lands back at the top of the page with no indication the transcript moved',
        );
      }
      // A page that changes under a screen reader without saying so is a page
      // that did not change, as far as the reader knows.
      const announced = await page.evaluate(() => {
        const live = Array.from(document.querySelectorAll('[aria-live], [role="status"]'));
        return live.map((node) => (node.textContent ?? '').replace(/\s+/g, ' ').trim());
      });
      if (!announced.some((text) => text.includes('201–250 of 250'))) {
        throw new Error(
          `the new page range is never announced; live regions on the page: ${JSON.stringify(announced)}`,
        );
      }
    },
  },
  {
    id: 'sessions-transcript-withheld',
    route: '/sessions',
    proves:
      'turns the store holds without their bodies, and a page that carried none of the session’s summary nodes, are both stated rather than drawn as empty',
    overrides: {
      [LCM_SESSION]: {
        status: 200,
        body: (() => {
          const base = structuredClone(resolveFixture(LCM_SESSION, '')) as Record<string, unknown>;
          const counts = base['counts'] as Record<string, unknown>;
          const messages = (base['messages'] as Record<string, unknown>[])
            .slice(0, 10)
            .map((message, i) => ({
              ...message,
              content: null,
              role: i === 0 ? null : 'assistant',
              timestamp: null,
              token_estimate: null,
              storage_kind: 'offloaded',
            }));
          return {
            ...base,
            messages,
            // The compactor cut this session, and this page of it carried none
            // of those cuts. That is a partial page, not a session the
            // compactor never touched.
            summary_nodes: [],
            has_more_summary_nodes: true,
            counts: { ...counts, message_count: 10, source_token_count: 0 },
          };
        })(),
      },
    },
    drive: openTranscript,
    assert: async (page) => {
      await expectVisibleText(
        page,
        'body not held by the store',
        'a turn whose body retention removed',
      );
      await expectVisibleText(page, 'role unrecorded', 'a turn with no recorded role');
      await expectVisibleText(page, 'no timestamp', 'a turn with no recorded time');
      // No compaction ratio exists against a zero source-token count, so none
      // is printed — the sentence takes the place of the figure.
      await expectVisibleText(
        page,
        'no compaction ratio exists to report',
        'the withheld ratio, explained',
      );
      await expectAbsent(page, 'text=Summaries hold', 'no ratio against a zero denominator');
      const partial = page.locator('[data-state="partial"]').first();
      if ((await partial.count()) === 0) {
        throw new Error('a page carrying none of the session’s summary nodes reported no state');
      }
      expectContains(
        (await partial.textContent()) ?? '',
        'this page carried no summary nodes',
        'the partial-page reason',
      );
    },
  },
];
