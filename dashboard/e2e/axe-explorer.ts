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
 * /explorer is deliberately absent from `axe-audit.ts`'s Plan 11 matrix subset:
 * this file already owns the surface's driven states, so the matrix runs here
 * instead of being reached for twice. Every state is swept at the three
 * showcase viewports in both themes, exactly as before; the two states a plain
 * arrival reaches — browse and searched — additionally carry the rest of the
 * plan's viewport, zoom and media matrix, because they are the states whose
 * long code signatures and dense result rows are what reflow actually strains.
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
import {
  FORCED_COLORS_PROBE,
  MIN_TOUCH_TARGET_PX,
  REFLOW_PROBE,
  RESPONSIVE_MATRIX,
  SHOWCASE_VIEWPORTS,
  TOUCH_TARGET_PROBE,
  clippedContentFailures,
  reflowFailures,
  touchTargetFailures,
  type ForcedColorsOptOut,
  type MediaMode,
  type ReflowReport,
  type Theme,
  type TouchTargetReport,
  type Viewport,
} from './responsive.ts';
import { ExplorerQueryRunV1Schema } from '../src/contracts/generated.ts';

const LABEL = process.argv[2] ?? 'current';
const OUT = path.join(process.cwd(), '.explorer-axe', LABEL);
const THEMES: readonly Theme[] = ['light', 'dark'];

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
  const parsed = ExplorerQueryRunV1Schema.safeParse((body as { payload?: unknown }).payload);
  if (!parsed.success) {
    throw new Error(`fixture ${label} fails ExplorerQueryRunV1Schema: ${parsed.error.message}`);
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
  viewport: string;
  width: number;
  height: number;
  zoom: number;
  media: MediaMode;
  violations: { id: string; impact: string | null; nodes: string[]; help: string }[];
  reflow: ReflowReport;
  targets: TouchTargetReport;
  forcedColorOptOuts?: ForcedColorsOptOut[];
  disabledRules?: string[];
  /** The plan assertions this combination failed, if any. */
  planFailures: string[];
}

const findings: Finding[] = [];
const planFailures: string[] = [];

/**
 * Scan and measure one state at one combination.
 *
 * `color-contrast` is off under forced colors for the reason recorded in
 * `responsive.ts`: axe-core 4.12.1 reads the authored palette rather than the
 * forced one, so it scores the dark theme as hundreds of failures the browser
 * has already corrected. The direct measurement of what forced colors can
 * genuinely break — elements declining the forced palette — replaces it.
 */
async function scan(
  page: Page,
  state: string,
  theme: Theme,
  viewport: Viewport,
  media: MediaMode,
): Promise<number> {
  const off = media === 'forced-colors' ? ['color-contrast'] : [];
  const builder = new AxeBuilder({ page }).withTags([
    'wcag2a',
    'wcag2aa',
    'wcag21a',
    'wcag21aa',
  ]);
  const results = await (off.length > 0 ? builder.disableRules(off) : builder).analyze();
  const reflow = (await page.evaluate(REFLOW_PROBE)) as ReflowReport;
  const targets = (await page.evaluate(TOUCH_TARGET_PROBE)) as TouchTargetReport;
  const optOuts =
    media === 'forced-colors'
      ? ((await page.evaluate(FORCED_COLORS_PROBE)) as ForcedColorsOptOut[])
      : [];
  const tag = `${state}/${theme}/${viewport.id}/${media}`;
  // Reflow is gated only where the plan gates it — 320 CSS pixels and 400%
  // zoom — and measured everywhere.
  const failures = [
    ...(viewport.reflowGated ? reflowFailures(reflow, tag) : []),
    ...(viewport.reflowGated ? clippedContentFailures(reflow, tag) : []),
    ...touchTargetFailures(targets, tag),
  ];
  planFailures.push(...failures);
  findings.push({
    state,
    theme,
    viewport: viewport.id,
    width: viewport.width,
    height: viewport.height,
    zoom: viewport.zoom,
    media,
    violations: results.violations.map((v) => ({
      id: v.id,
      impact: v.impact ?? null,
      help: v.help,
      nodes: v.nodes.map((n) => `${n.target.join(' ')} :: ${n.html.slice(0, 180)}`),
    })),
    reflow,
    targets,
    ...(optOuts.length ? { forcedColorOptOuts: optOuts } : {}),
    ...(off.length ? { disabledRules: off } : {}),
    planFailures: failures,
  });
  for (const f of failures) console.log(`      ! ${f}`);
  return results.violations.length;
}

async function shoot(page: Page, name: string): Promise<void> {
  await page.screenshot({ path: path.join(OUT, `${name}.png`), fullPage: true });
}

/**
 * Open a row, and distinguish the three things that can happen.
 *
 * Absent is not a finding: at 320 the inspector column is `max-md:hidden`, so
 * a state that renders no row there is a layout fact. PRESENT AND UNCLICKABLE
 * is a finding, and it is the one the plan names — "at 320 pixels and 400%
 * zoom there is no ... inaccessible action". A rendered control that cannot be
 * brought into view is exactly that, and the previous `click({ force: true })`
 * turned it into a fatal that abandoned every remaining state instead of
 * reporting it. The box and the capture go into the record so the claim can be
 * checked rather than taken on trust.
 */
async function openRow(page: Page, name: RegExp, tag: string): Promise<boolean> {
  const row = page.getByRole('button', { name }).first();
  if ((await row.count()) === 0) return false;
  try {
    // `force` because a sticky list caption can cover a row that was just
    // scrolled to; that overlap is itself a finding, not a harness problem.
    await row.click({ force: true, timeout: 10_000 });
  } catch (err) {
    const box = await row.boundingBox().catch(() => null);
    const size = page.viewportSize();
    // Whether ANY ancestor could have brought it into view. Without this the
    // report would be asserting unreachability from a failed click, and a
    // failed click is also what a Playwright quirk looks like.
    const reach = await row
      .evaluate((el) => {
        const chain: string[] = [];
        let room = 0;
        for (let up: Element | null = el.parentElement; up !== null; up = up.parentElement) {
          const style = getComputedStyle(up);
          const scrolls =
            (style.overflowY === 'auto' || style.overflowY === 'scroll') &&
            up.scrollHeight > up.clientHeight + 1;
          if (scrolls) room += up.scrollHeight - up.clientHeight - up.scrollTop;
          if (scrolls || up === document.body) {
            chain.push(
              `${up.tagName.toLowerCase()} overflowY=${style.overflowY} ` +
                `scrollH=${up.scrollHeight} clientH=${up.clientHeight} scrollTop=${up.scrollTop}`,
            );
          }
        }
        const doc = document.documentElement;
        return {
          remainingScroll: Math.round(room),
          documentScroll: doc.scrollHeight - doc.clientHeight,
          chain,
        };
      })
      .catch(() => null);
    const detail =
      `${tag}: the row matching ${String(name)} is rendered but cannot be activated. ` +
      `Viewport ${size?.width}x${size?.height}, row box ${JSON.stringify(box)}, ` +
      `scroll available ${JSON.stringify(reach)}. ` +
      `${String(err).split('\n')[0]}`;
    planFailures.push(detail);
    console.log(`      ! ${detail}`);
    await page.screenshot({
      path: path.join(OUT, `unreachable__${tag.replace(/[^\w-]+/g, '_')}.png`),
      fullPage: true,
    });
    return false;
  }
  await page.waitForTimeout(900);
  return true;
}

async function applyMedia(page: Page, media: MediaMode): Promise<void> {
  await page.emulateMedia({
    reducedMotion: 'reduce',
    contrast: media === 'contrast-more' ? 'more' : 'no-preference',
    forcedColors: media === 'forced-colors' ? 'active' : 'none',
  });
}

async function setTheme(page: Page, theme: Theme): Promise<void> {
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
  /** Land on /explorer fresh, in the given theme. */
  const arrive = async (viewport: Viewport, theme: Theme): Promise<void> => {
    await page.setViewportSize({ width: viewport.width, height: viewport.height });
    await page.goto(`${base}/explorer`, { waitUntil: 'domcontentloaded' });
    await setTheme(page, theme);
    await page.waitForSelector('main#td-main', { timeout: 20_000 });
    await page.waitForTimeout(900);
  };
  const search = async (term: string): Promise<void> => {
    const box = page.getByRole('searchbox').first();
    await box.click();
    await box.fill(term);
    await box.press('Enter');
    await page.waitForTimeout(1_200);
  };

  // The full state sweep, at the plan's three showcase viewports in both
  // themes. Unchanged in what it drives; the heights are now the plan's
  // (320x568, 768x1024, 1440x900) rather than a uniform 900.
  for (const viewport of SHOWCASE_VIEWPORTS) {
    for (const theme of THEMES) {
      console.log(`[axe] ${theme} @ ${viewport.id}`);
      await applyMedia(page, 'reduced-motion');
      await arrive(viewport, theme);

      total += await scan(page, 'browse', theme, viewport, 'reduced-motion');
      await shoot(page, `browse__${theme}__${viewport.id}`);

      await search('graph');
      total += await scan(page, 'searched', theme, viewport, 'reduced-motion');
      await shoot(page, `searched__${theme}__${viewport.id}`);

      const tag = `${theme}/${viewport.id}`;
      if (await openRow(page, /graph_search/, `inspector/${tag}`)) {
        total += await scan(page, 'inspector', theme, viewport, 'reduced-motion');
        await shoot(page, `inspector__${theme}__${viewport.id}`);
      }

      if (await openRow(page, /Using graph search/, `session-inspector/${tag}`)) {
        total += await scan(page, 'session-inspector', theme, viewport, 'reduced-motion');
        await shoot(page, `session__${theme}__${viewport.id}`);
      }

      await search('zzzz-no-such-token');
      total += await scan(page, 'no-rows-bounded', theme, viewport, 'reduced-motion');
      await shoot(page, `empty-bounded__${theme}__${viewport.id}`);

      await search('confirmed-no-such-token');
      total += await scan(page, 'no-rows-confirmed', theme, viewport, 'reduced-motion');
      await shoot(page, `empty-confirmed__${theme}__${viewport.id}`);
    }
  }

  // The rest of the plan matrix, over browse and searched. Grouped by (theme,
  // media) with a single arrival per group and a resize per viewport: every
  // mode here resolves through CSS, so re-searching thirty times would spend
  // the budget re-reaching a state the group already holds.
  const groups = new Map<string, typeof RESPONSIVE_MATRIX>();
  for (const c of RESPONSIVE_MATRIX) {
    const key = `${c.theme}|${c.media}`;
    groups.set(key, [...(groups.get(key) ?? []), c]);
  }
  for (const group of groups.values()) {
    const head = group[0]!;
    console.log(`[axe] matrix ${head.theme} / ${head.media}`);
    await applyMedia(page, head.media);
    await arrive(head.viewport, head.theme);
    for (const c of group) {
      await page.setViewportSize({ width: c.viewport.width, height: c.viewport.height });
      // Reflow and media-query re-evaluation need to settle before measuring.
      await page.waitForTimeout(450);
      total += await scan(page, 'browse', c.theme, c.viewport, c.media);
      await shoot(page, `browse__${c.theme}__${c.viewport.id}__${c.media}`);
    }
    // One searched pass per group, at the group's widest viewport: the result
    // list is where long code signatures meet a narrow column, and re-running
    // the query at every size in the group buys the same rows in a different
    // box for six times the wall clock.
    const widest = [...group].sort((a, b) => b.viewport.width - a.viewport.width)[0]!;
    await page.setViewportSize({ width: widest.viewport.width, height: widest.viewport.height });
    await search('graph');
    total += await scan(page, 'searched', widest.theme, widest.viewport, widest.media);
    await shoot(page, `searched__${widest.theme}__${widest.viewport.id}__${widest.media}`);
  }
  await applyMedia(page, 'reduced-motion');

  writeFileSync(path.join(OUT, 'findings.json'), `${JSON.stringify(findings, null, 2)}\n`);

  const byRule = new Map<string, number>();
  for (const f of findings)
    for (const v of f.violations) byRule.set(v.id, (byRule.get(v.id) ?? 0) + 1);

  console.log(`\n===== explorer axe (${LABEL}) =====`);
  for (const f of findings) {
    const tag = `${f.state}/${f.theme}/${f.viewport}/${f.media}`;
    if (f.violations.length === 0) {
      console.log(`  ${tag.padEnd(46)} 0`);
      continue;
    }
    console.log(`  ${tag.padEnd(46)} ${f.violations.length}`);
    for (const v of f.violations) {
      console.log(`      - ${v.id} [${v.impact}] x${v.nodes.length} — ${v.help}`);
      for (const n of v.nodes.slice(0, 3)) console.log(`          ${n}`);
    }
  }
  // One row per offending control rather than one per scan: the same 28px row
  // measured at thirty combinations is one defect, not thirty.
  const undersized = new Map<string, { size: string; name: string; scans: number; where: string }>();
  for (const f of findings) {
    for (const t of f.targets.undersized) {
      const seen = undersized.get(t.selector);
      if (seen === undefined) {
        undersized.set(t.selector, {
          size: `${t.width}x${t.height}`,
          name: t.name,
          scans: 1,
          where: `${f.state}/${f.theme}/${f.viewport}/${f.media}`,
        });
      } else seen.scans += 1;
    }
  }
  const optOuts = new Map<string, ForcedColorsOptOut>();
  for (const f of findings) for (const o of f.forcedColorOptOuts ?? []) optOuts.set(o.selector, o);

  console.log(`\n  scans=${findings.length}  totalViolations=${total}`);
  console.log(`  byRule=${JSON.stringify(Object.fromEntries(byRule))}`);
  console.log(
    `  plan touch targets (>= ${MIN_TOUCH_TARGET_PX}x${MIN_TOUCH_TARGET_PX} CSS px): ` +
      `${undersized.size} distinct control(s) under size`,
  );
  for (const [selector, t] of undersized) {
    console.log(
      `      ${t.size}  ${selector}${t.name === '' ? '' : `  "${t.name}"`}  in ${t.scans} scan(s), e.g. ${t.where}`,
    );
  }
  console.log(
    `  plan reflow (320 CSS px and 400% zoom): ` +
      `${planFailures.filter((f) => f.includes('scrolls horizontally')).length} failure(s)`,
  );
  console.log(
    `  forced colors: axe color-contrast disabled (it reads the authored palette, not the ` +
      `forced one); ${optOuts.size} element(s) decline the forced palette`,
  );
  for (const o of [...optOuts.values()].slice(0, 6)) {
    console.log(`      ${o.selector} color=${o.color} bg=${o.background}`);
  }
  console.log(`  planFailures=${planFailures.length}`);
  console.log(`  pageErrors=${pageErrors.length}`);
  console.log(`  shots=${OUT}`);

  await browser.close();
  server.close();
  // THE GATE. This used to be an unconditional `process.exit(0)`, so the run
  // above could report violations and still be read as a pass by anything that
  // checked the exit status — which is every CI runner and every reviewer who
  // trusted a green command. Nothing about the report changed; only that it can
  // now fail — and it now fails on the two Plan 11 measurements as well as on
  // axe, because a page that reflows into a sideways scroll or ships a 24px
  // control is inaccessible whether or not any axe rule names it.
  if (total > 0 || pageErrors.length > 0 || planFailures.length > 0) process.exitCode = 1;
}

main().catch((err: unknown) => {
  console.error('[axe] fatal:', err);
  process.exit(1);
});
