/**
 * Every scenario the axe gate drives, and what each one is evidence for.
 *
 * `npx tsx e2e/axe-audit.ts`  (run from `dashboard/`)
 *
 * Settings, Knowledge, Delivery, Loom and Agents live in `axe-workspaces.ts`,
 * which explains why they are separate; their canaries stay here with the rest.
 *
 * A plain navigation reaches none of the states that matter here: a governance
 * read that FAILED, a health read that could not be resolved, a coordinator that
 * claims complete coverage over units it never examined. Each scenario overrides
 * only the route under test, drives the surface into one state, ASSERTS what the
 * surface then claims, and scans it.
 *
 * The assertions are the point. An earlier harness read the Doctor dot's state
 * into a JSON file and never compared it to anything; its three dot scenarios
 * were silently exercising one state for months, because its fixture did not
 * match the shape the nav rail parses. Reading a state without asserting on it
 * is how three tests become one.
 *
 * THAT THIS GATE CAN FAIL IS NOW PROVEN BY THE RUN, not by a procedure someone
 * remembers to follow. The `canary` factory plants known-inaccessible markup on
 * a real surface at every viewport and theme and requires the scan to report
 * it; see it for why the check runs on every scan, and once per audited ROUTE,
 * rather than once per run.
 *
 * Where the scenarios spend their attention, and why:
 *
 *   the states nobody looks at. An `unsupported` panel, a metric whose value
 *   does not exist, a page of a transcript that carried none of the session's
 *   summary nodes, a mount that is `ready` and separately `unauthorized` — these
 *   render least often and are reviewed least carefully, so they are where
 *   accessibility regressions survive. Most of them are unreachable by
 *   navigation, which is why each one overrides its route.
 *
 *   the reading, not the markup. Axe cannot tell whether a unit reached the
 *   accessibility tree beside the figure it scales, whether a group header's
 *   tally agrees with its own plates, or whether activating a pager left focus
 *   anywhere at all. Those are asserted directly — see
 *   `assertMeasurementIsSelfDescribing`, `assertMetricPlateTruth`, and
 *   `sessions-transcript-paged`.
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
  runHarness,
  searchFor,
  type Scenario,
} from './axe-harness.ts';
import { WORKSPACE_SCENARIOS } from './axe-workspaces.ts';

const STORAGE_FINDINGS_KINDS = [
  'over_budget_store',
  'orphan_store',
  'stale_branch_dbs',
  'incident_debris_present',
  'retention_backlog',
] as const;

/**
 * The storage-findings envelope with each producer's source coverage replaced.
 *
 * Built from the checked-in fixture rather than hand-rolled, so the envelope
 * and Doctor payload around `kind_statuses` stay exactly what the audit and the
 * `endpoint-fixtures` contract gate already validate. A hand-written envelope
 * that silently failed to parse would make every scenario read `unknown` and
 * look like a passing three-state dot while proving nothing.
 */
function storageFindings(
  statuses: ReadonlyArray<{ state: string; observed_entries: number }>,
): Record<string, unknown> {
  const base = structuredClone(
    resolveFixture('/api/storage/findings', '') as {
      payload: Record<string, unknown>;
      [k: string]: unknown;
    },
  );
  base.payload['kind_statuses'] = STORAGE_FINDINGS_KINDS.map((kind, i) => ({
    kind,
    state: statuses[i]!.state,
    observed_entries: statuses[i]!.observed_entries,
    reason: `${kind} source coverage reported as ${statuses[i]!.state}`,
  }));
  return base;
}

function allProducers(state: string, observed = 0) {
  return STORAGE_FINDINGS_KINDS.map(() => ({ state, observed_entries: observed }));
}

/** The scheduler payload, with the two review queues in a chosen state. */
function scheduler(review: {
  factProposals: { state: 'measured'; count: number } | { state: 'unreadable'; reason: string };
  skills: { state: 'measured'; count: number } | { state: 'unreadable'; reason: string };
}): Record<string, unknown> {
  const wire = (r: typeof review.factProposals) =>
    r.state === 'measured'
      ? { state: 'measured', count: r.count, reason: null }
      : { state: 'unreadable', count: null, reason: r.reason };
  return {
    status: 'configured',
    paused: false,
    enabled: true,
    scheduler_tick_secs: 900,
    // Null, never zero, for a queue that could not be read — exactly what
    // `automation_scheduler_api.rs` now emits.
    pending_fact_proposals:
      review.factProposals.state === 'measured' ? review.factProposals.count : null,
    pending_skills: review.skills.state === 'measured' ? review.skills.count : null,
    pending_review: { fact_proposals: wire(review.factProposals), skills: wire(review.skills) },
    now: Math.floor(Date.now() / 1000),
    last_session_activity: Math.floor(Date.now() / 1000) - 1200,
    project_config_path: '/fast/projects/tracedecay/.tracedecay/automation.toml',
    control_path: '/fast/projects/tracedecay/.tracedecay/automation.control.json',
    tasks: [
      { task: 'memory_curator', due: false, skip_reason: 'cooldown', last_scheduler_run: null },
      { task: 'session_reflector', due: true, skip_reason: null, last_scheduler_run: null },
      { task: 'skill_writer', due: false, skip_reason: 'no_new_sessions', last_scheduler_run: null },
    ],
  };
}

const SCHEDULER = '/api/automation/scheduler/status';
const FINDINGS = '/api/storage/findings';
const EXPLORER_QUERIES = '/api/explorer/queries';

/** One Explorer source's coverage, defaulting to a fully accounted denominator. */
function sourceCoverage(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    completeness: 'complete',
    eligible: 0,
    examined: 0,
    matched: 0,
    excluded: 0,
    omitted: 0,
    unknown: 0,
    denominator: 0,
    unit: 'rows',
    omission_reasons: [],
    ...over,
  };
}

function explorerSource(
  id: 'code_graph' | 'sessions' | 'knowledge',
  label: string,
  coverage: Record<string, unknown>,
): Record<string, unknown> {
  return {
    source_id: id,
    source_label: label,
    phase: 'completed',
    outcome: 'ready',
    completed_units: 0,
    total_units: coverage['denominator'],
    coverage,
    freshness: 'unknown',
    watermark: null,
    error_code: null,
    message: null,
    page: { offset: 0, limit: 50, total: 0, next_offset: null, rows: [], metadata: {} },
  };
}

/**
 * A coordinator run that answered with no rows, for the term the scenario
 * searches.
 *
 * `finality` is `complete` in every case here on purpose: the point of these
 * scenarios is that the coordinator's summary scalar says "canonical", and the
 * surface must still read the per-source unit accounting underneath it before
 * repeating that as a global-absence claim.
 */
function explorerEmptyRun(query: string, sources: unknown[]): Record<string, unknown> {
  const base = structuredClone(
    resolveFixture('/api/storage/findings', '') as { payload: unknown; [k: string]: unknown },
  );
  return {
    ...base,
    domain_state: 'ready',
    payload: {
      // Keyed per query: the status poll looks up by `run_id` alone, so a shared
      // id makes the client serve a previous run, discard it as belonging to
      // another query, and wait forever.
      run_id: `ui-truth-${Buffer.from(query).toString('hex').slice(0, 12)}`,
      request: { query, limit: 50, offset: 0 },
      request_revision: 'explorer-query-request-v1',
      plan_revision: 'explorer-query-plan-v1',
      merge_revision: 'source-local-no-merge-v1',
      required_source_ids: ['code_graph', 'sessions', 'knowledge'],
      ordering_policy: 'source_local_no_cross_source_merge',
      explanation: 'Search each required source and preserve its own order and coverage.',
      submitted_at_micros: 1,
      completed_at_micros: 4_100,
      elapsed_micros: 4_099,
      state: 'completed',
      finality: 'complete',
      sources,
    },
  };
}

/** The pending-review tiles, as rendered text keyed by label. */
async function reviewTiles(page: Page): Promise<Record<string, string>> {
  return page.evaluate(() => {
    const out: Record<string, string> = {};
    for (const legend of Array.from(document.querySelectorAll('.td-legend'))) {
      const label = (legend.textContent ?? '').trim();
      if (label !== 'pending proposals' && label !== 'pending skills') continue;
      const cell = legend.parentElement?.querySelector('[data-cell="numeric"]');
      out[label] = (cell?.textContent ?? '').trim();
    }
    return out;
  });
}

async function doctorDotState(page: Page): Promise<{ state: string; label: string }> {
  const dot = page.locator('[data-doctor-health]').first();
  await dot.waitFor({ state: 'attached', timeout: 15_000 });
  return {
    state: (await dot.getAttribute('data-doctor-health')) ?? '',
    label: (await dot.getAttribute('aria-label')) ?? '',
  };
}

/**
 * Two rules, chosen because they are detected by different means: `image-alt`
 * is a missing-attribute check, `button-name` runs axe's accessible-name
 * computation. A scan that reports both is exercising more than one code path
 * inside the engine.
 */
const SEEDED_DEFECTS = ['image-alt', 'button-name'] as const;

/**
 * Plant the known-inaccessible markup the canary scan must find.
 *
 * Injected into the live page rather than served as a static fixture so the
 * proof runs through the *same* path every real scenario uses — this build's
 * bundle, this context, this stillness init, this `AxeBuilder` tag set. A
 * separate hand-written HTML page would prove that axe works somewhere, which
 * is not the question; the question is whether these scans would have caught
 * anything.
 */
async function seedKnownDefects(page: Page): Promise<void> {
  const planted = await page.evaluate(() => {
    const main = document.querySelector('main#td-main');
    if (!main) return -1;
    const host = document.createElement('div');
    host.setAttribute('data-axe-canary', '');
    // A 1x1 transparent GIF inlined as a data URI: the scan must never depend
    // on a network fetch, and axe only needs the element, not the pixels.
    const img = document.createElement('img');
    img.src = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';
    img.style.cssText = 'width:24px;height:24px';
    host.append(img);
    // No text, no aria-label, no title: nothing for the accessible-name
    // computation to find.
    const button = document.createElement('button');
    button.type = 'button';
    button.style.cssText = 'width:24px;height:24px';
    host.append(button);
    main.prepend(host);
    return host.childElementCount;
  });
  if (planted !== SEEDED_DEFECTS.length) {
    throw new Error(
      `the canary planted ${planted} elements, expected ${SEEDED_DEFECTS.length}` +
        (planted === -1 ? ' (main#td-main was not in the page)' : ''),
    );
  }
}

/**
 * The canary, bound to one route.
 *
 * One canary per audited route rather than one per run. The engine is not
 * route-scoped, so a single canary does prove the scan can see a defect — but
 * it proves it about the page it ran on. Every route reaches `analyze()`
 * through its own render: a surface that throws during hydration, or renders
 * nothing an accessibility scan can reach, produces the same `violations: 0` as
 * a clean one. Seeding on each route means the zero recorded for that route was
 * measured on that route.
 */
function canary(
  id: string,
  route: string,
  drive?: (page: Page) => Promise<void>,
  // `matrix` canaries carry the Plan 11 viewport/zoom/media matrix for their
  // routes, so every 390x844, 400%-zoom and forced-colors scan in the run is
  // one where a planted violation had to be reported for the scan to count.
  // A widened matrix whose new combinations silently stopped detecting
  // anything would otherwise read as five more routes scoring clean. Five of
  // these are enough to witness every combination in the run, so a canary
  // added purely for a new route's own liveness may stay on the showcase tier.
  tier: 'matrix' | 'showcase' = 'matrix',
): Scenario {
  return {
    id,
    route,
    proves: `THE GATE ITSELF on ${route} — a known-inaccessible element is reported here, so this route's zeros are measurements`,
    overrides: {},
    matrix: tier === 'matrix',
    // Checked on every scan, not once: the seeding is re-applied after each
    // navigation, so each viewport and theme is an independent confirmation
    // that the scan running at that size is live. It also means a breakage
    // that only shows up at one width cannot hide behind five good scans.
    expectViolations: SEEDED_DEFECTS,
    drive: async (page) => {
      if (drive !== undefined) await drive(page);
      await seedKnownDefects(page);
    },
    assert: async (page) => {
      // The planted nodes must really be on the page and really be visible. An
      // element the browser never laid out is one axe skips, which would turn
      // the whole canary into a check that cannot fail.
      const planted = page.locator('[data-axe-canary]');
      const hosts = await planted.count();
      if (hosts !== 1) throw new Error(`expected one canary host in the page, found ${hosts}`);
      if (!(await planted.isVisible())) {
        throw new Error('the canary markup is present but not visible, so axe would skip it');
      }
    },
  };
}

const OBSERVATORY = '/api/observatory';
const COSTS = '/api/costs';
const FRESHNESS = '/api/code-index/freshness';
/** The transcript drill-down. The trailing slash keeps the override off the
 * sibling `/sessions` list route, which is a different payload entirely. */
const LCM_SESSION = '/api/plugins/hermes-lcm/session/';

/**
 * A checked-in envelope fixture with its envelope and payload edited in place.
 *
 * Cloned rather than constructed, for the same reason `storageFindings` is:
 * `DashboardEnvelopeV1` carries scope, version, time, watermark, coverage,
 * freshness, authorization and legal actions, and an envelope missing one of
 * them fails `EnvelopeSchema` and arrives as `unsupported_schema`. Every
 * scenario below would then render the same schema notice, scan clean, and
 * prove nothing about the state it named.
 */
function envelopeFixture(
  pathname: string,
  edit: (envelope: Record<string, unknown>, payload: Record<string, unknown>) => void,
): Record<string, unknown> {
  const base = structuredClone(resolveFixture(pathname, '')) as Record<string, unknown>;
  const payload = base['payload'];
  if (typeof payload !== 'object' || payload === null) {
    throw new Error(`the ${pathname} fixture carries no payload object to edit`);
  }
  edit(base, payload as Record<string, unknown>);
  return base;
}

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

/** The freshness fixture's own mounted worktree, with fields replaced. */
function mountedWorktree(over: Record<string, unknown>): Record<string, unknown> {
  const fixture = envelopeFixture(FRESHNESS, () => {});
  const payload = fixture['payload'] as Record<string, unknown>;
  const worktrees = payload['worktrees'] as Record<string, unknown>[];
  return { ...worktrees[0]!, ...over };
}

/** A freshness read in one of the route's five states. */
function freshness(spec: {
  state: string;
  note: string;
  worktrees?: unknown[];
  authorization?: 'authorized' | 'denied' | 'redacted' | 'unauthorized';
}): Record<string, unknown> {
  return envelopeFixture(FRESHNESS, (envelope, payload) => {
    envelope['domain_state'] = spec.state;
    envelope['authorization'] = { outcome: spec.authorization ?? 'authorized' };
    payload['worktrees'] = spec.worktrees ?? [];
    payload['note'] = spec.note;
  });
}

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
function openTranscript(page: Page): Promise<void> {
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

/**
 * The invariants every Plan 26 metric plate must hold, checked against what the
 * read actually carried rather than against a count written into this file.
 *
 * Asserting "7 of 9 measured" as a literal would pass for the wrong reasons the
 * moment a fixture changed. What matters is internal consistency: each group
 * header must agree with its own plates, and no plate may print a figure its
 * value does not support.
 */
async function assertMetricPlateTruth(page: Page, what: string): Promise<void> {
  const report = await page.evaluate(() => {
    const problems: string[] = [];
    let plates = 0;
    let unavailable = 0;
    for (const group of Array.from(document.querySelectorAll('[data-metric-source]'))) {
      const source = group.getAttribute('data-metric-source') ?? '?';
      const inGroup = Array.from(group.querySelectorAll('[data-metric]'));
      let measured = 0;
      for (const plate of inGroup) {
        plates += 1;
        const id = plate.getAttribute('data-metric') ?? '?';
        const figure = (plate.querySelector('[data-cell="numeric"]')?.textContent ?? '').trim();
        if (plate.getAttribute('data-metric-available') === 'true') {
          measured += 1;
          if (figure === '—' || figure === '') {
            problems.push(`${id}: carries a value but printed ${JSON.stringify(figure)}`);
          }
          continue;
        }
        unavailable += 1;
        // The whole point of the contract: a measurement that does not exist
        // must not become a zero, an empty string, or any other figure.
        if (figure !== '—') {
          problems.push(`${id}: has no value but printed ${JSON.stringify(figure)}`);
        }
        const chip = plate.querySelector('[data-state="unknown"]');
        if (chip === null) {
          problems.push(`${id}: has no value and no unknown chip`);
          continue;
        }
        const reason = (chip.parentElement?.textContent ?? '').replace(/unknown/i, '').trim();
        if (reason === '') problems.push(`${id}: has no value and no reason`);
      }
      const tally = Array.from(group.querySelectorAll('span'))
        .map((span) => (span.textContent ?? '').trim())
        .find((text) => /^\d+ of \d+ measured$/.test(text));
      const expected = `${measured} of ${inGroup.length} measured`;
      if (tally !== expected) {
        problems.push(`${source}: header ${JSON.stringify(tally)} disagrees with its plates (${expected})`);
      }
    }
    return { problems, plates, unavailable };
  });
  if (report.plates === 0) throw new Error(`${what}: no metric plates rendered at all`);
  if (report.problems.length > 0) throw new Error(`${what}: ${report.problems.join(' | ')}`);
  console.log(
    `[axe]              ${what}: ${report.plates} plates, ${report.unavailable} without a value`,
  );
}

/**
 * The text one plate exposes to assistive technology, in order, with
 * `aria-hidden` and undisplayed subtrees removed.
 *
 * This is the measurable form of "programmatically associated, not merely
 * visually adjacent". A unit drawn by CSS, or hidden from the accessibility
 * tree, or lifted out of the list item that owns the figure, leaves a screen
 * reader announcing a bare number — and no axe rule reports it, because
 * nothing in the markup is malformed.
 */
async function exposedPlateText(page: Page, metric: string): Promise<string[]> {
  return page.evaluate((id) => {
    const plate = document.querySelector(`[data-metric="${id}"]`);
    if (plate === null) return [];
    const parts: string[] = [];
    // Depth-first with an explicit stack, children pushed in reverse so they
    // pop in document order. Not a recursive inner function: esbuild's
    // `keepNames` rewrites a named function expression into a call to a
    // `__name` helper that does not exist inside the page, and the evaluate
    // then dies with `ReferenceError: __name is not defined`.
    const stack: Node[] = [plate];
    while (stack.length > 0) {
      const node = stack.pop()!;
      if (node.nodeType === Node.TEXT_NODE) {
        const text = (node.textContent ?? '').trim();
        if (text !== '') parts.push(text);
        continue;
      }
      if (node.nodeType !== Node.ELEMENT_NODE) continue;
      const element = node as Element;
      if (element.getAttribute('aria-hidden') === 'true') continue;
      if (getComputedStyle(element).display === 'none') continue;
      const children = Array.from(element.childNodes);
      for (let i = children.length - 1; i >= 0; i -= 1) stack.push(children[i]!);
    }
    return parts;
  }, metric);
}

/** One plate's printed figure, and whether the wire gave it a value at all. */
async function plateReading(
  page: Page,
  metric: string,
): Promise<{ figure: string; available: string }> {
  return page.evaluate((id) => {
    const plate = document.querySelector(`[data-metric="${id}"]`);
    if (plate === null) return { figure: '(no plate)', available: '(no plate)' };
    return {
      figure: (plate.querySelector('[data-cell="numeric"]')?.textContent ?? '').trim(),
      available: plate.getAttribute('data-metric-available') ?? '(unset)',
    };
  }, metric);
}

/** A figure, its unit and its labelled denominator must all reach assistive
 * technology, in that order, from inside the one list item that is the
 * measurement. */
async function assertMeasurementIsSelfDescribing(
  page: Page,
  metric: string,
  expected: { figure: string; unit: string; denominator: string },
): Promise<void> {
  const parts = await exposedPlateText(page, metric);
  if (parts.length === 0) throw new Error(`${metric}: no plate on the page to read`);
  const at = (needle: string) => parts.findIndex((part) => part.includes(needle));
  const figure = at(expected.figure);
  const unit = at(expected.unit);
  const denominator = at(expected.denominator);
  const term = at('denominator');
  const missing = [
    figure < 0 ? `figure ${JSON.stringify(expected.figure)}` : '',
    unit < 0 ? `unit ${JSON.stringify(expected.unit)}` : '',
    denominator < 0 ? `denominator ${JSON.stringify(expected.denominator)}` : '',
    term < 0 ? 'the word "denominator"' : '',
  ].filter((entry) => entry !== '');
  if (missing.length > 0) {
    throw new Error(
      `${metric}: ${missing.join(', ')} never reached the accessibility tree. ` +
        `Exposed: ${JSON.stringify(parts)}`,
    );
  }
  if (unit < figure) {
    throw new Error(
      `${metric}: the unit is announced before its figure, so the reading arrives out of order. ` +
        `Exposed: ${JSON.stringify(parts)}`,
    );
  }
  if (term > denominator) {
    throw new Error(
      `${metric}: the denominator value is announced before the term that names it. ` +
        `Exposed: ${JSON.stringify(parts)}`,
    );
  }
}

const SCENARIOS: readonly Scenario[] = [
  canary('axe-engine-canary', '/automations'),
  canary('observatory-canary', '/observatory'),
  canary('costs-canary', '/costs'),
  canary('code-canary', '/code'),
  canary('sessions-canary', '/sessions', openTranscript),
  {
    id: 'automations-measured',
    route: '/automations',
    proves: 'a real count still renders as a measured figure',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        body: scheduler({
          factProposals: { state: 'measured', count: 5 },
          skills: { state: 'measured', count: 2 },
        }),
      },
    },
    assert: async (page) => {
      const tiles = await reviewTiles(page);
      expectEqual(tiles['pending proposals'], '5', 'measured proposals tile');
      expectEqual(tiles['pending skills'], '2', 'measured skills tile');
      await expectVisibleText(page, 'measured', 'measured evidence pattern');
      await expectAbsent(
        page,
        'text=Awaiting-review counts are unknown',
        'no unknown banner on a measured read',
      );
    },
  },
  {
    id: 'automations-confirmed-empty',
    route: '/automations',
    proves: 'a queue that was read and is genuinely empty may still say zero',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        body: scheduler({
          factProposals: { state: 'measured', count: 0 },
          skills: { state: 'measured', count: 0 },
        }),
      },
    },
    assert: async (page) => {
      const tiles = await reviewTiles(page);
      expectEqual(tiles['pending proposals'], '0', 'confirmed-empty proposals tile');
      expectEqual(tiles['pending skills'], '0', 'confirmed-empty skills tile');
      await expectAbsent(
        page,
        'text=Awaiting-review counts are unknown',
        'a confirmed empty queue is not reported as unknown',
      );
    },
  },
  {
    id: 'automations-unreadable',
    route: '/automations',
    proves: 'THE DEFECT 1 PROOF — both governance queues failed to read, and neither renders 0',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        body: scheduler({
          factProposals: {
            state: 'unreadable',
            reason: 'the project fact authority could not be read: database is locked',
          },
          skills: {
            state: 'unreadable',
            reason: 'the managed skill store could not be read: permission denied',
          },
        }),
      },
    },
    assert: async (page) => {
      const tiles = await reviewTiles(page);
      // The whole defect, asserted directly: a failed read must not print a 0.
      expectEqual(tiles['pending proposals'], '—', 'unreadable proposals tile');
      expectEqual(tiles['pending skills'], '—', 'unreadable skills tile');
      if (tiles['pending proposals'] === '0' || tiles['pending skills'] === '0') {
        throw new Error('FALSIFIED: a failed governance read rendered as a measured zero');
      }
      await expectVisibleText(page, 'unknown', 'unknown evidence pattern');
      await expectVisibleText(
        page,
        'Awaiting-review counts are unknown, not zero.',
        'the unknown-not-zero sentence',
      );
      await expectVisibleText(page, 'database is locked', 'the fact-authority failure reason');
      await expectVisibleText(page, 'permission denied', 'the skill-store failure reason');
    },
  },
  {
    id: 'automations-mixed',
    route: '/automations',
    proves: 'one unreadable queue never suppresses the other queue’s real count',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        body: scheduler({
          factProposals: { state: 'measured', count: 3 },
          skills: {
            state: 'unreadable',
            reason: 'the user profile root could not be resolved: no home directory',
          },
        }),
      },
    },
    assert: async (page) => {
      const tiles = await reviewTiles(page);
      expectEqual(tiles['pending proposals'], '3', 'measured proposals beside an unreadable queue');
      expectEqual(tiles['pending skills'], '—', 'unreadable skills tile');
      await expectVisibleText(page, 'no home directory', 'the profile-root failure reason');
    },
  },
  {
    id: 'automations-uncontracted-payload',
    route: '/automations',
    proves:
      'a scheduler payload missing the pending_review union reads as unsupported schema, never as counts',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        // The flat `pending_*` mirrors without the discriminated union. The
        // bundle ships inside the binary that answers this route, so this is
        // not a version skew the surface may paper over with the mirrors: it
        // is a payload that does not satisfy the generated contract.
        body: {
          status: 'configured',
          paused: false,
          enabled: true,
          scheduler_tick_secs: 900,
          pending_fact_proposals: null,
          pending_skills: null,
          now: Math.floor(Date.now() / 1000),
          last_session_activity: null,
          project_config_path: '/x/automation.toml',
          control_path: '/x/automation.control.json',
          tasks: [],
        },
      },
    },
    assert: async (page) => {
      await expectVisibleText(
        page,
        'The daemon answered with a shape this build does not understand.',
        'the unsupported-schema sentence',
      );
      // No tile at all is the point: an uncontracted payload must not be
      // mined for a number, and must not print a zero in place of one.
      const tiles = await reviewTiles(page);
      if ('pending proposals' in tiles || 'pending skills' in tiles) {
        throw new Error(
          `FALSIFIED: an uncontracted scheduler payload still rendered review tiles: ${JSON.stringify(tiles)}`,
        );
      }
      await expectAbsent(
        page,
        'text=Awaiting-review counts are unknown',
        'no partial scheduler panel behind an unsupported-schema state',
      );
    },
  },
  {
    id: 'brain',
    route: '/brain',
    proves: 'Brain definition lists are well-formed (definition-list / dlitem)',
    overrides: {},
    // Brain is the one audited route with no canary, and its definition lists
    // are the densest reflow subject in the app, so it carries the matrix for
    // /brain. Its zeros lean on the five canaried routes scanning the same
    // combinations in the same run rather than on a planted defect of its own.
    matrix: true,
    assert: async (page) => {
      // Structural check independent of axe: every dt/dd sits directly in a dl
      // or in a div wrapper whose parent is the dl, and each group's dt
      // precedes its dd in DOM order.
      const bad = await page.evaluate(() => {
        const problems: string[] = [];
        for (const dl of Array.from(document.querySelectorAll('dl'))) {
          for (const child of Array.from(dl.children)) {
            const tag = child.tagName;
            if (tag !== 'DT' && tag !== 'DD' && tag !== 'DIV') {
              problems.push(`dl has a ${tag} child`);
            }
            if (tag === 'DIV') {
              const kids = Array.from(child.children)
                .map((k) => k.tagName)
                .filter((t) => t !== 'SPAN');
              const dtAt = kids.indexOf('DT');
              const ddAt = kids.indexOf('DD');
              if (dtAt >= 0 && ddAt >= 0 && dtAt > ddAt) {
                problems.push(`dd precedes dt in a dl group: ${kids.join(',')}`);
              }
            }
          }
        }
        return problems;
      });
      if (bad.length > 0) throw new Error(`malformed definition lists: ${bad.join(' | ')}`);
      const dlCount = await page.locator('dl').count();
      if (dlCount === 0) throw new Error('Brain rendered no definition lists at all');
    },
  },
  {
    id: 'brain-scoped',
    route: '/brain',
    proves: 'the per-project Brain reached from the registry is also well-formed',
    overrides: {},
    // Scoping to a project swaps in a different set of definition lists, which
    // is why the registry view passing does not vouch for this one.
    drive: (page) => openRow(page, /tracedecay/i),
    assert: async (page) => {
      const dlCount = await page.locator('dl').count();
      if (dlCount === 0) throw new Error('scoped Brain rendered no definition lists at all');
    },
  },
  {
    id: 'explorer-absence-confirmed',
    route: '/explorer',
    drive: (page) => searchFor(page, 'confirmed-absent-token'),
    proves: 'a genuinely empty index can still report itself as empty',
    overrides: {
      [EXPLORER_QUERIES]: {
        status: 200,
        body: explorerEmptyRun('confirmed-absent-token', [
          explorerSource('code_graph', 'Code graph', sourceCoverage({ unit: 'symbols' })),
          explorerSource('sessions', 'Sessions', sourceCoverage()),
          explorerSource('knowledge', 'Knowledge', sourceCoverage({ unit: 'facts' })),
        ]),
      },
    },
    assert: async (page) => {
      await expectVisibleText(page, 'No source matched', 'the confirmed-absence heading');
      await expectVisibleText(page, 'examined its full denominator', 'the confirmed-absence reason');
      await expectVisibleText(page, 'measured', 'measured evidence pattern');
    },
  },
  {
    id: 'explorer-absence-all-unknown',
    route: '/explorer',
    drive: (page) => searchFor(page, 'all-unknown-token'),
    proves:
      'THE DEFECT PROOF — a source whose every unit is unknown cannot yield a known-coverage claim',
    overrides: {
      [EXPLORER_QUERIES]: {
        status: 200,
        body: explorerEmptyRun('all-unknown-token', [
          explorerSource('code_graph', 'Code graph', sourceCoverage({ unit: 'symbols' })),
          explorerSource('sessions', 'Sessions', sourceCoverage()),
          explorerSource(
            'knowledge',
            'Knowledge',
            sourceCoverage({
              eligible: 5,
              examined: 5,
              unknown: 5,
              denominator: 5,
              unit: 'facts',
              omission_reasons: ['every unit resolved to unknown status'],
            }),
          ),
        ]),
      },
    },
    assert: async (page) => {
      await expectVisibleText(
        page,
        'could not determine the status of any of its 5 facts',
        'the all-unknown refusal reason',
      );
      await expectAbsent(
        page,
        'text=completed with known coverage',
        'no known-coverage claim when every unit is unknown',
      );
      await expectAbsent(page, 'text=No source matched', 'no confirmed-absence heading');
      await expectVisibleText(page, 'unknown', 'unknown evidence pattern');
    },
  },
  {
    id: 'explorer-absence-examined-nothing',
    route: '/explorer',
    drive: (page) => searchFor(page, 'examined-nothing-token'),
    proves: 'THE DEFECT PROOF — a source that examined nothing cannot yield a completed claim',
    overrides: {
      [EXPLORER_QUERIES]: {
        status: 200,
        body: explorerEmptyRun('examined-nothing-token', [
          explorerSource(
            'code_graph',
            'Code graph',
            sourceCoverage({
              eligible: 400,
              examined: 0,
              omitted: 400,
              denominator: 400,
              unit: 'symbols',
              omission_reasons: ['result cap reached before any unit was examined'],
            }),
          ),
          explorerSource('sessions', 'Sessions', sourceCoverage()),
          explorerSource('knowledge', 'Knowledge', sourceCoverage({ unit: 'facts' })),
        ]),
      },
    },
    assert: async (page) => {
      await expectVisibleText(
        page,
        'examined none of its 400 symbols',
        'the examined-nothing refusal reason',
      );
      await expectAbsent(
        page,
        'text=completed with known coverage',
        'no known-coverage claim when nothing was examined',
      );
    },
  },
  {
    id: 'navrail-healthy',
    route: '/automations',
    proves: 'DEFECT 2 state 1 — every producer looked and found nothing: verified healthy',
    overrides: { [FINDINGS]: { status: 200, body: storageFindings(allProducers('real', 0)) } },
    assert: async (page) => {
      const dot = await doctorDotState(page);
      expectEqual(dot.state, 'healthy', 'doctor dot state');
      expectContains(dot.label, 'measured healthy', 'doctor dot label');
    },
  },
  {
    id: 'navrail-attention',
    route: '/automations',
    proves: 'DEFECT 2 state 2 — a producer observed real findings: attention needed',
    overrides: {
      [FINDINGS]: {
        status: 200,
        body: storageFindings([
          { state: 'real', observed_entries: 3 },
          ...allProducers('real', 0).slice(1),
        ]),
      },
    },
    assert: async (page) => {
      const dot = await doctorDotState(page);
      expectEqual(dot.state, 'attention', 'doctor dot state');
      expectContains(dot.label, 'need attention', 'doctor dot label');
    },
  },
  {
    id: 'navrail-unknown-transport',
    route: '/automations',
    proves: 'DEFECT 2 state 3 — the storage-findings read is broken: health unknown, not all-clear',
    overrides: {
      [FINDINGS]: { status: 500, body: { detail: 'storage findings reader unavailable' } },
    },
    assert: async (page) => {
      const dot = await doctorDotState(page);
      expectEqual(dot.state, 'unknown', 'doctor dot state');
      expectContains(dot.label, 'health unknown', 'doctor dot label');
    },
  },
  {
    id: 'navrail-unknown-nocoverage',
    route: '/automations',
    proves: 'a producer that never ran is unknown, not a clean bill of health',
    overrides: {
      [FINDINGS]: { status: 200, body: storageFindings(allProducers('unsupported', 0)) },
    },
    assert: async (page) => {
      const dot = await doctorDotState(page);
      expectEqual(dot.state, 'unknown', 'doctor dot state');
    },
  },

  /* ==========================================================================
   * Plan 26 canonical observations — Observatory.
   * ========================================================================== */
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

  /* ==========================================================================
   * Plan 26 canonical cost observations, and the latency panel that has no
   * measurement behind it — Costs.
   * ========================================================================== */
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

  /* ==========================================================================
   * Branch-aware code-index freshness — Code. Five server states, four of which
   * are only reachable by overriding the route.
   * ========================================================================== */
  {
    id: 'code-freshness-fresh',
    route: '/code',
    proves: 'a sealed generation with complete coverage names the source reference it is a picture of',
    overrides: {},
    assert: async (page) => {
      const panel = page.locator('[data-index-freshness]');
      expectEqual(
        (await panel.getAttribute('data-index-freshness')) ?? '',
        'ready',
        'the freshness domain state',
      );
      await expectVisibleText(
        page,
        'refs/heads/codex/tracedecay-total-redesign-plan',
        'the branch the generation was sealed against',
      );
      await expectAbsent(
        page,
        '[data-state="unauthorized"]',
        'no authorization chip on an authorized read',
      );
    },
  },
  {
    id: 'code-freshness-unsupported',
    route: '/code',
    proves:
      'no daemon scheduler registry at all is `unsupported` — there is no generation to report, and no fresh badge is drawn',
    overrides: {
      [FRESHNESS]: {
        status: 200,
        body: freshness({
          state: 'unsupported',
          note: 'no daemon-owned scheduler registry is attached to this dashboard, so no sealed generation can be reported',
        }),
      },
    },
    assert: async (page) => {
      expectEqual(
        (await page.locator('[data-index-freshness]').getAttribute('data-index-freshness')) ?? '',
        'unsupported',
        'the freshness domain state',
      );
      await expectVisibleText(
        page,
        'no daemon-owned scheduler registry is attached',
        "the route's own note, which is the only thing separating this from an unmounted project",
      );
      await expectAbsent(page, '[data-worktree-staleness]', 'no worktree readings to show');
    },
  },
  {
    id: 'code-freshness-no-mount',
    route: '/code',
    proves:
      'a registry that is attached with nothing mounted for this project is `unknown` — the same empty list, a different claim',
    overrides: {
      [FRESHNESS]: {
        status: 200,
        body: freshness({
          state: 'unknown',
          note: 'a scheduler registry is attached but holds no mounted scheduler for this project',
        }),
      },
    },
    assert: async (page) => {
      expectEqual(
        (await page.locator('[data-index-freshness]').getAttribute('data-index-freshness')) ?? '',
        'unknown',
        'the freshness domain state',
      );
      await expectVisibleText(
        page,
        'holds no mounted scheduler for this project',
        'the note that distinguishes this from an absent registry',
      );
    },
  },
  {
    id: 'code-freshness-indexing',
    route: '/code',
    proves: 'a mount that is still indexing is `loading`, not stale and not ready',
    overrides: {
      [FRESHNESS]: {
        status: 200,
        body: freshness({
          state: 'loading',
          note: 'a scheduler is mounted and indexing; no generation has been sealed yet',
          worktrees: [
            mountedWorktree({
              latest_generation_id: null,
              snapshot_content_identity: null,
              sealed_at_micros: null,
              staleness_state: null,
              coverage: 'indexing',
              hook_hint_count: 41,
            }),
          ],
        }),
      },
    },
    assert: async (page) => {
      expectEqual(
        (await page.locator('[data-index-freshness]').getAttribute('data-index-freshness')) ?? '',
        'loading',
        'the freshness domain state',
      );
      // Every absent identity field says it is absent. None of them become an
      // epoch date or an empty cell.
      await expectVisibleText(page, 'no sealed generation yet', 'the unsealed generation');
      await expectVisibleText(page, 'not reported', 'the unreported staleness and stamps');
      await expectAbsent(page, 'text=1970-01-01', 'no epoch date standing in for an absent stamp');
    },
  },
  {
    id: 'code-freshness-incomplete-coverage',
    route: '/code',
    proves:
      'a sealed generation whose coverage is incomplete is `partial` — the generation exists and does not cover everything',
    overrides: {
      [FRESHNESS]: {
        status: 200,
        body: freshness({
          state: 'partial',
          note: 'the sealed generation exists and the scheduler reports incomplete coverage of it',
          worktrees: [
            mountedWorktree({
              coverage: 'incomplete',
              staleness_state: 'stale',
              source_reference: 'refs/heads/master',
              hook_hint_count: 128,
            }),
          ],
        }),
      },
    },
    assert: async (page) => {
      expectEqual(
        (await page.locator('[data-index-freshness]').getAttribute('data-index-freshness')) ?? '',
        'partial',
        'the freshness domain state',
      );
      expectEqual(
        (await page.locator('[data-worktree-staleness]').first().getAttribute(
          'data-worktree-staleness',
        )) ?? '',
        'stale',
        'the worktree staleness',
      );
      // The branch-aware part: a generation sealed against master while the
      // checkout is elsewhere is stale in a way no node count reveals.
      await expectVisibleText(page, 'refs/heads/master', 'the reference the generation is of');
      await expectVisibleText(page, 'incomplete', 'the coverage shortfall');
    },
  },
  {
    id: 'code-freshness-unauthorized',
    route: '/code',
    proves:
      'THE AUTHORIZATION AXIS — a mount that is ready and separately unauthorized shows both states',
    overrides: {
      [FRESHNESS]: {
        status: 200,
        body: freshness({
          state: 'ready',
          authorization: 'unauthorized',
          note: 'the mount is sealed and current; the asking identity is not authorized for its contents',
          worktrees: [mountedWorktree({})],
        }),
      },
    },
    assert: async (page) => {
      const panel = page.locator('[data-index-freshness]');
      expectEqual(
        (await panel.getAttribute('data-index-freshness')) ?? '',
        'ready',
        'the freshness domain state is untouched by the authorization outcome',
      );
      const unauthorized = panel.locator('[data-state="unauthorized"]').first();
      if ((await unauthorized.count()) === 0) {
        throw new Error('FALSIFIED: an unauthorized read rendered no unauthorized chip');
      }
      expectContains(
        (await unauthorized.textContent()) ?? '',
        'read authorization',
        'the unauthorized chip names its axis',
      );
      if ((await panel.locator('[data-state="ready"]').count()) === 0) {
        throw new Error(
          'FALSIFIED: the authorization outcome replaced the domain state instead of joining it',
        );
      }
    },
  },

  /* ==========================================================================
   * The transcript drill-down — Sessions. The inspector column is
   * `max-md:hidden`, so these scan the drill-down at 768 and 1440 and the
   * bare list at 320.
   * ========================================================================== */
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

  /* ==========================================================================
   * The thirteenth channel, whose data plane is deliberately closed — Work.
   *
   * Work is routed and navigable and has no generated read model behind it, so
   * the only claim it makes is about its own contract inventory. That makes it
   * the easiest surface in the app to get wrong in the direction this whole
   * gate exists to catch: an uncontracted workspace that quietly starts drawing
   * lanes, figures and controls it never read. No override is needed — the
   * surface issues no request at all, which is itself part of the claim.
   * ========================================================================== */
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

  // The five workspaces this gate did not visit. Their scenarios live in
  // `axe-workspaces.ts`; the canaries stay here beside the other five so the
  // per-route liveness rule is legible in one place.
  //
  // Showcase-only, deliberately. A canary answers "did THIS route render
  // something a scan can see", which is a question about hydration and not
  // about width, and the showcase tier already asks it at 320, 768 and 1440 in
  // both themes. The thirty-combination tier is carried by the five canaries
  // above, so every 390x844, 400%-zoom, contrast-more and forced-colors scan in
  // the run is still one where a planted violation had to be reported.
  ...WORKSPACE_SCENARIOS,
  canary('settings-canary', '/settings', undefined, 'showcase'),
  canary('knowledge-canary', '/knowledge', undefined, 'showcase'),
  canary('delivery-canary', '/delivery', undefined, 'showcase'),
  canary('loom-canary', '/loom', undefined, 'showcase'),
  canary('agents-canary', '/agents', undefined, 'showcase'),
  canary('work-canary', '/work', undefined, 'showcase'),
];

runHarness(SCENARIOS).catch((err: unknown) => {
  console.error('[axe] fatal:', err);
  process.exit(1);
});
