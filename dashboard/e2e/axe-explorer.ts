/**
 * Explorer-only axe + screenshot harness.
 *
 * Scratch tool: the shared `stories/audit.ts` rm -rf's `audit-gallery/` and
 * pins port 5173, and peer agents are running it concurrently. This runs the
 * same axe configuration (wcag2a + wcag2aa, plus wcag21a/wcag21aa which the
 * shared harness omits) against /explorer only, on its own port and output
 * directory, and drives the surface through browse / searched / inspector so
 * the states a plain navigation never reaches are scanned too.
 *
 *   npm run axe:explorer              # from `dashboard/`
 *   npm run axe:explorer -- <label>   # output subdirectory under `.explorer-axe/`
 */
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { chromium, type Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { installApiFixtures } from '../stories/fixtures/route.ts';
import { STILLNESS_INIT, startStaticServer } from './axe-harness.ts';
import { ExplorerQueryRunSchema } from '../src/contracts/wire.ts';

const LABEL = process.argv[2] ?? 'current';
const OUT = path.join(process.cwd(), '.explorer-axe', LABEL);
const WIDTHS = [320, 768, 1440] as const;
const THEMES = ['light', 'dark'] as const;

const CODE_ROWS = [
  {
    id: 'node-1',
    name: 'graph_search',
    kind: 'function',
    file_path: 'src/dashboard/graph_service.rs',
    signature: 'pub fn graph_search(query: &str, limit: usize) -> Vec<SymbolHit>',
    degree: 42,
  },
  {
    id: 'node-2',
    name: 'GraphService',
    kind: 'struct',
    file_path: 'src/dashboard/graph_service.rs',
    signature: 'pub struct GraphService',
    degree: 31,
  },
  {
    id: 'node-3',
    name: 'search_symbols_bounded',
    kind: 'function',
    file_path: 'src/graph/query/search.rs',
    signature: 'fn search_symbols_bounded(scope: &ResolvedScope) -> BoundedPage<SymbolHit>',
    degree: 18,
  },
  {
    id: 'node-4',
    name: 'SymbolHit',
    kind: 'struct',
    file_path: 'src/graph/model/hit.rs',
    signature: 'pub struct SymbolHit',
    degree: 9,
  },
];
const MESSAGE_ROWS = [
  {
    message_id: 'message-1',
    session_id: 'session-1',
    source: 'cursor',
    role: 'assistant',
    snippet: 'Using graph search to locate the bounded page reader before editing.',
    created_at_micros: 1_760_000_000_000_000,
  },
  {
    message_id: 'message-2',
    session_id: 'session-1',
    source: 'claude',
    role: 'user',
    snippet: 'Why does graph search skip the vector generation when it is stale?',
    created_at_micros: 1_759_900_000_000_000,
  },
];
const SUMMARY_ROWS = [
  {
    node_id: 'summary-1',
    session_id: 'session-2',
    summary: 'Graph route investigation across the dashboard planner and the daemon gateway.',
  },
];
const FACT_ROWS = [
  {
    fact_id: 11,
    content: 'Graph search is bounded by ResolvedScope and never crosses project stores.',
    category: 'project',
    trust_score: 0.82,
  },
  {
    fact_id: 12,
    content: 'Explorer never merges source results into one ranked list.',
    category: 'design',
    trust_score: 0.64,
  },
];

function coverage(total: number | null, unit: string) {
  return {
    completeness: total === null ? 'unknown' : 'complete',
    eligible: total,
    examined: total,
    matched: total,
    excluded: total === null ? null : 0,
    omitted: total === null ? null : 0,
    unknown: total === null ? null : 0,
    denominator: total,
    unit,
    omission_reasons: total === null ? ['matching fact total is not exposed'] : [],
  };
}

function sourceProgress(
  id: string,
  label: string,
  rows: Record<string, unknown>[],
  total: number | null,
  unit: string,
) {
  return {
    source_id: id,
    source_label: label,
    phase: 'completed',
    outcome: 'ready',
    completed_units: rows.length,
    total_units: total,
    coverage: coverage(total, unit),
    freshness: 'unknown',
    watermark: null,
    error_code: null,
    message: null,
    page: { offset: 0, limit: 50, total, next_offset: null, rows, metadata: {} },
  };
}

function envelope(payload: unknown, domainState: string) {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'profile_sharded', store_root: '/data' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 10 },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'partial',
      eligible: 3,
      examined: 2,
      matched: null,
      excluded: null,
      omitted: 1,
      unknown: null,
      denominator: 3,
      unit: 'sources',
      omission_reasons: ['knowledge coverage is unknown'],
    },
    freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
    domain_state: domainState,
    legal_actions: [],
    payload,
  };
}

const PLANNER_RUN = envelope(
  {
    run_id: 'explorer-run-4f2c9ab1',
    request: { query: 'graph', limit: 50, offset: 0 },
    request_revision: 'explorer-query-request-v1',
    plan_revision: 'explorer-query-plan-v1',
    merge_revision: 'source-local-no-merge-v1',
    required_source_ids: ['code_graph', 'sessions', 'knowledge'],
    ordering_policy: 'source_local_no_cross_source_merge',
    explanation:
      'Search the code graph, active-project session store, and bounded project fact authority in parallel; preserve each source own order and coverage.',
    submitted_at_micros: 1,
    completed_at_micros: 9_400,
    elapsed_micros: 9_399,
    state: 'partial',
    finality: 'partial',
    sources: [
      sourceProgress('code_graph', 'Code graph', CODE_ROWS, 4, 'symbols'),
      sourceProgress('sessions', 'Sessions', [...MESSAGE_ROWS, ...SUMMARY_ROWS], 3, 'rows'),
      {
        ...sourceProgress('knowledge', 'Knowledge', FACT_ROWS, null, 'rows'),
        outcome: 'unavailable',
        error_code: 'fact_store_unavailable',
        message: 'the bounded project fact authority is not mounted for this project',
        page: null,
        completed_units: null,
        total_units: null,
      },
    ],
  },
  'partial',
);

const SESSION_SIZE = envelope(
  {
    session_id: 'session-1',
    storage_scope: 'profile_sharded',
    counts: {
      message_count: 412,
      summary_node_count: 18,
      token_estimate_total: 184_320,
      summary_token_count: 9_140,
      source_token_count: 175_180,
    },
  },
  'ready',
);

const READ_CONTEXT = envelope(
  {
    session_id: 'session-1',
    storage_scope: 'profile_sharded',
    limit: 25,
    offset: 0,
    order: 'asc',
    counts: {
      message_count: 412,
      summary_node_count: 18,
      token_estimate_total: 184_320,
      summary_token_count: 9_140,
      source_token_count: 175_180,
    },
    messages: MESSAGE_ROWS,
    summary_nodes: SUMMARY_ROWS,
    has_more: true,
    has_more_messages: true,
    has_more_summary_nodes: false,
  },
  'partial',
);

const EXPLORER_FIXTURES: [RegExp, unknown][] = [
  [/\/api\/explorer\/sessions\/[^/]+\/size/, SESSION_SIZE],
  [/\/api\/explorer\/sessions\/[^/]+\/read-context/, READ_CONTEXT],
  [/\/api\/explorer\/queries/, PLANNER_RUN],
  [/\/api\/plugins\/graph\/overview/, { top_connected: CODE_ROWS }],
  [
    /\/api\/plugins\/hermes-lcm\/overview/,
    { latest_summary_nodes: SUMMARY_ROWS, overview: { messages_total: 412 } },
  ],
  [
    /\/api\/plugins\/holographic\//,
    { limit: 25, holographic: { path: '/data/memory.db', exists: true, error: '', facts: FACT_ROWS } },
  ],
];

/**
 * A fixture that fails the real contract is silently indistinguishable from a
 * product bug: the client rejects the envelope and falls back to `pending`, so
 * the surface sits in a loading state and the audit scans the wrong thing.
 * Every planner fixture is parsed with the schema the component itself uses
 * before any scanning starts.
 */
function assertFixtureParses(label: string, body: unknown): void {
  const parsed = ExplorerQueryRunSchema.safeParse((body as { payload?: unknown }).payload);
  if (!parsed.success) {
    throw new Error(`fixture ${label} fails ExplorerQueryRunSchema: ${parsed.error.message}`);
  }
}

let lastQuery = 'graph';

function requestedQuery(body: string | null): string | null {
  if (body === null) return null;
  try {
    const parsed = JSON.parse(body) as { query?: unknown };
    return typeof parsed.query === 'string' ? parsed.query : null;
  } catch {
    return null;
  }
}

/** `confirmed` distinguishes the two honest zero states: every source complete
 * over a real denominator (a confirmed global absence) versus bounded pages
 * that cannot establish absence at all. */
function lastQueryWasUnmatched(query: string): boolean {
  return query.startsWith('confirmed-');
}

function emptyRun(query: string, confirmed: boolean): unknown {
  const zero = (id: string, label: string, unit: string) => ({
    ...sourceProgress(id, label, [], 0, unit),
    coverage: {
      ...coverage(0, unit),
      completeness: confirmed ? 'complete' : 'unknown',
      denominator: confirmed ? 0 : null,
    },
  });
  return envelope(
    {
      // The status query is keyed on `run_id` alone, so reusing one id across
      // queries makes the client serve the previous run from cache, discard it
      // as belonging to another query, and wait forever.
      run_id: `explorer-run-${Buffer.from(query).toString('hex').slice(0, 12)}`,
      request: { query, limit: 50, offset: 0 },
      request_revision: 'explorer-query-request-v1',
      plan_revision: 'explorer-query-plan-v1',
      merge_revision: 'source-local-no-merge-v1',
      required_source_ids: ['code_graph', 'sessions', 'knowledge'],
      ordering_policy: 'source_local_no_cross_source_merge',
      explanation:
        'Search the code graph, active-project session store, and bounded project fact authority in parallel; preserve each source own order and coverage.',
      submitted_at_micros: 1,
      completed_at_micros: 4_100,
      elapsed_micros: 4_099,
      state: confirmed ? 'completed' : 'partial',
      finality: confirmed ? 'complete' : 'partial',
      sources: [
        zero('code_graph', 'Code graph', 'symbols'),
        zero('sessions', 'Sessions', 'rows'),
        zero('knowledge', 'Knowledge', 'rows'),
      ],
    },
    confirmed ? 'ready' : 'partial',
  );
}

interface Finding {
  state: string;
  theme: string;
  width: number;
  violations: { id: string; impact: string | null; nodes: string[]; help: string }[];
}

const findings: Finding[] = [];

async function scan(page: Page, state: string, theme: string, width: number): Promise<number> {
  const results = await new AxeBuilder({ page })
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
    .analyze();
  findings.push({
    state,
    theme,
    width,
    violations: results.violations.map((v) => ({
      id: v.id,
      impact: v.impact ?? null,
      help: v.help,
      nodes: v.nodes.map((n) => `${n.target.join(' ')} :: ${n.html.slice(0, 180)}`),
    })),
  });
  return results.violations.length;
}

async function shoot(page: Page, name: string): Promise<void> {
  await page.screenshot({ path: path.join(OUT, `${name}.png`), fullPage: true });
}

async function setTheme(page: Page, theme: string): Promise<void> {
  await page.evaluate((t) => {
    try {
      localStorage.setItem('td-theme', t);
    } catch {
      /* storage disabled */
    }
    document.documentElement.dataset['theme'] = t;
  }, theme);
}

async function main(): Promise<void> {
  mkdirSync(OUT, { recursive: true });
  // The built bundle over a static server, not `rsbuild dev`: lazy route
  // compilation under the dev server can race and throw, rendering the router's
  // accessible error boundary, which screenshots happily and passes Axe.
  const { baseURL: base, server } = startStaticServer();
  const reap = () => {
    server.close();
  };
  process.on('exit', reap);
  process.on('SIGINT', () => {
    reap();
    process.exit(130);
  });

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ deviceScaleFactor: 1 });
  const page = await context.newPage();
  // A crashed route renders accessible markup and scores a clean scan, so a
  // page error is a run failure rather than a log line.
  const pageErrors: string[] = [];
  page.on('pageerror', (error) => {
    pageErrors.push(error.message);
    console.error(`[axe] PAGEERROR ${error.message}`);
  });
  if (process.env.EXPLORER_AXE_TRACE) {
    page.on('console', (message) => console.log(`[page:${message.type()}] ${message.text()}`));
  }
  await installApiFixtures(page);
  // Registered after the generic fixture route so Playwright (last match wins)
  // prefers these Explorer-specific payloads.
  await page.route('**/api/explorer/**', async (route) => {
    const url = route.request().url();
    // The planner echoes the query it was actually asked for, so a query that
    // matches nothing produces a genuine zero-row run instead of a run the
    // client discards as stale (which would leave the surface pending forever
    // and hide the empty-state presentation from this audit).
    if (/\/api\/explorer\/queries/.test(url)) {
      // The status poll is a GET on `/queries/{run_id}` and carries no body, so
      // the query has to be remembered from the POST. Answering the poll with
      // the wrong query is what made the client treat every run as stale and
      // sit in `pending` forever.
      const posted = requestedQuery(route.request().postData());
      if (posted !== null) lastQuery = posted;
      const asked = lastQuery;
      if (process.env.EXPLORER_AXE_TRACE) {
        console.log(`[route] ${route.request().method()} ${url} asked=${asked}`);
      }
      const body =
        asked === 'graph' ? PLANNER_RUN : emptyRun(asked, lastQueryWasUnmatched(asked));
      assertFixtureParses(asked, body);
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(body),
      });
      return;
    }
    const hit = EXPLORER_FIXTURES.find(([re]) => re.test(url));
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(hit ? hit[1] : {}),
    });
  });
  for (const [re, payload] of EXPLORER_FIXTURES) {
    if (String(re).includes('explorer')) continue;
    await page.route(
      (url) => re.test(url.toString()),
      async (route) =>
        route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify(payload),
        }),
    );
  }
  // Stillness by REMOVING animation, never by shortening it: an animation with
  // `both` fill and zero duration pins its from-state and the capture is blank.
  await page.addInitScript({ content: STILLNESS_INIT });

  let total = 0;
  for (const width of WIDTHS) {
    await page.setViewportSize({ width, height: 900 });
    for (const theme of THEMES) {
      console.log(`[axe] ${theme} @ ${width}`);
      await page.goto(`${base}/explorer`, { waitUntil: 'domcontentloaded' });
      await setTheme(page, theme);
      await page.waitForSelector('main#td-main', { timeout: 20_000 });
      await page.waitForTimeout(900);

      total += await scan(page, 'browse', theme, width);
      await shoot(page, `browse__${theme}__${width}`);

      const box = page.getByRole('searchbox').first();
      await box.click();
      await box.fill('graph');
      await box.press('Enter');
      await page.waitForTimeout(1_200);
      total += await scan(page, 'searched', theme, width);
      await shoot(page, `searched__${theme}__${width}`);

      // `force` because a sticky list caption can cover a row that was just
      // scrolled to; that overlap is itself a finding, not a harness problem.
      const row = page.getByRole('button', { name: /graph_search/ }).first();
      if ((await row.count()) > 0) {
        await row.click({ force: true });
        await page.waitForTimeout(900);
        total += await scan(page, 'inspector', theme, width);
        await shoot(page, `inspector__${theme}__${width}`);
      }

      const sessionRow = page.getByRole('button', { name: /Using graph search/ }).first();
      if ((await sessionRow.count()) > 0) {
        await sessionRow.click({ force: true });
        await page.waitForTimeout(900);
        total += await scan(page, 'session-inspector', theme, width);
        await shoot(page, `session__${theme}__${width}`);
      }

      await box.fill('zzzz-no-such-token');
      await box.press('Enter');
      await page.waitForTimeout(1_200);
      total += await scan(page, 'no-rows-bounded', theme, width);
      await shoot(page, `empty-bounded__${theme}__${width}`);

      await box.fill('confirmed-no-such-token');
      await box.press('Enter');
      await page.waitForTimeout(1_200);
      total += await scan(page, 'no-rows-confirmed', theme, width);
      await shoot(page, `empty-confirmed__${theme}__${width}`);
    }
  }

  writeFileSync(path.join(OUT, 'findings.json'), `${JSON.stringify(findings, null, 2)}\n`);

  const byRule = new Map<string, number>();
  for (const f of findings)
    for (const v of f.violations) byRule.set(v.id, (byRule.get(v.id) ?? 0) + 1);

  console.log(`\n===== explorer axe (${LABEL}) =====`);
  for (const f of findings) {
    const tag = `${f.state}/${f.theme}/${f.width}`;
    if (f.violations.length === 0) {
      console.log(`  ${tag.padEnd(34)} 0`);
      continue;
    }
    console.log(`  ${tag.padEnd(34)} ${f.violations.length}`);
    for (const v of f.violations) {
      console.log(`      - ${v.id} [${v.impact}] x${v.nodes.length} — ${v.help}`);
      for (const n of v.nodes.slice(0, 3)) console.log(`          ${n}`);
    }
  }
  console.log(`\n  scans=${findings.length}  totalViolations=${total}`);
  console.log(`  byRule=${JSON.stringify(Object.fromEntries(byRule))}`);
  console.log(`  pageErrors=${pageErrors.length}`);
  console.log(`  shots=${OUT}`);

  await browser.close();
  server.close();
  // THE GATE. This used to be an unconditional `process.exit(0)`, so the run
  // above could report violations and still be read as a pass by anything that
  // checked the exit status — which is every CI runner and every reviewer who
  // trusted a green command. Nothing about the report changed; only that it can
  // now fail.
  if (total > 0 || pageErrors.length > 0) process.exitCode = 1;
}

main().catch((err: unknown) => {
  console.error('[axe] fatal:', err);
  process.exit(1);
});
