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
 * The default fixtures serve the whole LCM family as `unavailable` because
 * temporal retrieval is not mounted in the fixture daemon — an honest default,
 * but one with no transcript in it to audit. So every scenario here overrides
 * BOTH routes it depends on: the overview that feeds the session list, and the
 * session read behind the drill-down. The payloads are built to the generated
 * wire contract (`LcmOverviewPayloadV1`, `LcmSessionPayloadV1`), inside the
 * same canonical envelope the daemon serves, and the pager is driven by the
 * server's opaque `next_cursor` — offset paging no longer exists on this
 * route, and a harness that still sent `offset=` would audit a request the
 * surface never makes.
 *
 * `openTranscript` is the one export that faces outward. `sessions-canary`
 * plants its markup in the drill-down rather than in the list behind it, so the
 * canary and these scenarios have to open the surface the same way; it lives
 * here, with the route knowledge it encodes, and `axe-canary.ts` imports it.
 */
import type { Page } from '@playwright/test';
import { fixtureEnvelope } from '../src/test/fixtureEnvelope.ts';
import {
  expectAbsent,
  expectContains,
  expectEqual,
  expectVisibleText,
  focusedElement,
  openRow,
  type Scenario,
} from './axe-harness.ts';

const LCM_OVERVIEW = '/api/plugins/hermes-lcm/overview';
/** The transcript drill-down. The trailing slash keeps the override off the
 * sibling `/sessions` list route, which is a different payload entirely. */
const LCM_SESSION = '/api/plugins/hermes-lcm/session/';

const SESSION_ID = 'claude:reconcile-2026-07-21';
const PAGE_SIZE = 100;

/** The list the drill-down opens from. `openTranscript` matches the date in
 * the session id, so the id here and the regex there move together. */
function overviewEnvelope(messageCount: number): Record<string, unknown> {
  return fixtureEnvelope({
    exists: true,
    path: 'daemon://lcm',
    storage_scope: 'project',
    query: '',
    limit: 30,
    latest_sessions: [
      {
        session_id: SESSION_ID,
        message_count: messageCount,
        last_timestamp: 1_752_990_400,
        last_store_id: 90_000 + messageCount,
      },
    ],
    latest_summary_nodes: [],
    matches: { messages: [], summary_nodes: [] },
    overview: {
      sessions_total: 1,
      messages_total: messageCount,
      summary_nodes_total: 3,
      summary_node_sessions_total: 1,
      max_summary_depth: 2,
      depth_counts: [
        { depth: 1, count: 2 },
        { depth: 2, count: 1 },
      ],
      role_counts: [
        { role: 'assistant', count: Math.ceil((messageCount * 2) / 3) },
        { role: 'user', count: Math.floor(messageCount / 3) },
      ],
      source_counts: [{ source: 'claude', count: messageCount }],
      compression: {
        node_count: 3,
        ratio: null,
        source_token_count: 18_400,
        token_count: 1_020,
      },
    },
  });
}

/** One turn at the exact `LcmMessageV1` shape. */
function turn(ordinal: number, total: number): Record<string, unknown> {
  return {
    message_id: `${SESSION_ID}:${String(ordinal).padStart(4, '0')}`,
    session_id: SESSION_ID,
    role: ordinal % 3 === 0 ? 'user' : 'assistant',
    content: `turn ${ordinal + 1} of ${total}`,
    snippet: null,
    ordinal,
    timestamp: null,
    tool_name: null,
    token_count: 18 + (ordinal % 9) * 7,
    token_count_provenance: 'o200k_approximate',
    pinned: 0,
    source: 'claude',
    storage_kind: 'message',
    store_id: 90_000 + ordinal,
    summary_node_ids: [],
    metadata_json: null,
  };
}

/** The compactor's cuts, at the exact `LcmSummaryNodeV1` shape. Three of
 * them, because the first scenario counts the boundary rows. */
function summaryNodes(): Record<string, unknown>[] {
  return [
    {
      node_id: 'sn-recon-0001',
      session_id: SESSION_ID,
      category: 'tool_activity',
      depth: 1,
      summary:
        'Read and grepped the worktree reconciliation path, then confirmed the durable restart branch against the scheduler registry.',
      snippet: null,
      source_type: 'messages',
      source_token_count: 9_800,
      token_count: 540,
      created_at: 1_752_988_400,
      latest_at: 1_752_989_050,
      recency: null,
      expand_hint: 'lcm expand node:sn-recon-0001',
    },
    {
      node_id: 'sn-recon-0002',
      session_id: SESSION_ID,
      category: 'code_change',
      depth: 1,
      summary:
        'Edits to the generation-publication path and the reconciliation guard, with the tests that cover them.',
      snippet: null,
      source_type: 'messages',
      source_token_count: 6_200,
      token_count: 330,
      created_at: 1_752_989_600,
      latest_at: 1_752_990_100,
      recency: null,
      expand_hint: 'lcm expand node:sn-recon-0002',
    },
    {
      node_id: 'sn-recon-0003',
      session_id: SESSION_ID,
      category: 'outcome',
      depth: 2,
      summary:
        'Session outcome: reconciliation verified, one gap left open against the hook-hint queue drain.',
      snippet: null,
      source_type: 'summary_nodes',
      source_token_count: 2_400,
      token_count: 150,
      created_at: 1_752_990_400,
      latest_at: null,
      recency: null,
      expand_hint: 'lcm expand node:sn-recon-0003',
    },
  ];
}

/** One `LcmSessionPayloadV1` page inside the canonical envelope. */
function sessionEnvelope(
  page: Partial<Record<string, unknown>> & { messages: unknown[] },
): Record<string, unknown> {
  return fixtureEnvelope({
    exists: true,
    session_id: SESSION_ID,
    path: 'daemon://session-temporal',
    storage_scope: 'project',
    limit: PAGE_SIZE,
    counts: {
      message_count: 46,
      source_token_count: 18_400,
      summary_node_count: 3,
      summary_token_count: 1_020,
    },
    summary_nodes: summaryNodes(),
    has_more: false,
    has_more_messages: false,
    has_more_summary_nodes: false,
    next_cursor: null,
    ...page,
  });
}

/**
 * A transcript served as REAL server pages behind opaque cursors: the reply
 * carries `next_cursor` when more turns follow, and the next request presents
 * that cursor back. The cursor is opaque to the surface but not to this
 * server, which encodes the continuation offset in it.
 *
 * A fixed body would answer every cursor with page one, so the pager would
 * appear to work while nothing moved — and the focus assertion below would be
 * measuring a frozen fixture rather than the surface's behaviour when the
 * control it was activated from disables itself.
 */
function transcriptPages(total: number): (url: URL) => Record<string, unknown> {
  return (url) => {
    const cursor = url.searchParams.get('cursor');
    const offset = cursor == null ? 0 : Number(cursor.replace('at-', ''));
    const limit = Number(url.searchParams.get('limit') ?? String(PAGE_SIZE));
    const served = Math.max(0, Math.min(limit, total - offset));
    const more = offset + served < total;
    return sessionEnvelope({
      limit,
      counts: {
        message_count: total,
        source_token_count: 18_400,
        summary_node_count: 3,
        summary_token_count: 1_020,
      },
      messages: Array.from({ length: served }, (_, i) => turn(offset + i, total)),
      has_more: more,
      has_more_messages: more,
      next_cursor: more ? `at-${offset + served}` : null,
    });
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
    overrides: {
      [LCM_OVERVIEW]: { status: 200, body: overviewEnvelope(46) },
      [LCM_SESSION]: {
        status: 200,
        body: sessionEnvelope({
          messages: Array.from({ length: 46 }, (_, i) => turn(i, 46)),
        }),
      },
    },
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
    overrides: {
      [LCM_OVERVIEW]: { status: 200, body: overviewEnvelope(250) },
      [LCM_SESSION]: { status: 200, bodyFor: transcriptPages(250) },
    },
    drive: async (page) => {
      await openTranscript(page);
      await pageForward(page, /page 2 /);
      await pageForward(page, /page 3 /);
    },
    assert: async (page) => {
      // The read really advanced: this is a server page behind a real cursor,
      // not the first page relabelled.
      await expectVisibleText(page, 'page 3', 'the last page number');
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
      if (!announced.some((text) => text.includes('page 3'))) {
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
      [LCM_OVERVIEW]: { status: 200, body: overviewEnvelope(10) },
      [LCM_SESSION]: {
        status: 200,
        body: sessionEnvelope({
          messages: Array.from({ length: 10 }, (_, i) => ({
            ...turn(i, 10),
            content: null,
            role: i === 0 ? null : 'assistant',
            timestamp: null,
            token_count: null,
            token_count_provenance: null,
            storage_kind: 'offloaded',
          })),
          // The compactor cut this session, and this page of it carried none
          // of those cuts. That is a partial page, not a session the
          // compactor never touched.
          summary_nodes: [],
          has_more_summary_nodes: true,
          counts: {
            message_count: 10,
            source_token_count: 0,
            summary_node_count: 3,
            summary_token_count: 1_020,
          },
        }),
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
