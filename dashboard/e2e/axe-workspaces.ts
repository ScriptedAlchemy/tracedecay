/**
 * The five workspaces the axe gate did not reach: Settings, Knowledge,
 * Delivery, Loom, Agents.
 *
 * Plan 11 requires all twelve workspaces to be complete, navigable and
 * accessible. The gate had scenarios for seven, and the gap was not academic:
 * a hand sweep of all twelve at all eight plan viewports found a third
 * collapsed-scroller site — `/settings` at 400% zoom trapping 4,388px of
 * configuration behind a live "N settings" count — plus five undersized
 * controls, all of the same class the gate already fails on and all invisible
 * only because no scenario visited the page. A gate covering seven of twelve
 * under-reports by construction.
 *
 * Own module rather than more of `axe-audit.ts`, for two reasons: that file was
 * already 1,585 lines, and these five surfaces need payload builders nothing
 * else uses. `/api/loom/temporal` and `/api/delivery/overview` have no entry in
 * the fixture registry at all — `resolveFixture` answers `{}` for both, which
 * the surfaces correctly render as an undecodable schema — so those two
 * scenarios build a contracted payload and mount it inside a real envelope
 * cloned from a checked-in fixture.
 *
 * The discipline is `axe-audit.ts`'s: drive the surface into one state and
 * ASSERT WHAT IT THEN CLAIMS. Each scenario below picks the place its route
 * makes a truth claim, because that is where a falsified reading hides:
 *
 *   /settings   the "N settings" count against the rows actually rendered —
 *               the count that made the trapped pane invisible — and the
 *               review dialog's promise to send only the changed fields.
 *   /knowledge  a trust histogram that came back all-zero must fall through to
 *               a source that carries mass, and say which, rather than drawing
 *               ten empty bars captioned "every fact in the store".
 *   /delivery   a typed unavailable projection must stay unavailable and never
 *               become "0 pull requests".
 *   /loom       an end time nobody recorded must not become a zero duration,
 *               and a session with no start must be stated as absent from the
 *               field rather than silently dropped.
 *   /agents     an unreadable analytics diagnostics read must print no figure,
 *               and must not suppress the usage read that did answer.
 *
 * Nothing here hard-codes a figure it could instead check for consistency: the
 * settings count is compared to the rendered rows, Loom's measured-extent
 * fraction to its own table, Knowledge's total to the sum of its own bands. A
 * literal from a fixture passes for the wrong reason the moment the fixture
 * moves.
 */
import type { Page } from '@playwright/test';
import { resolveFixture } from '../stories/fixtures/data.ts';
import {
  expectContains,
  expectEqual,
  expectVisibleText,
  type Scenario,
} from './axe-harness.ts';

const MEMORY_BASE = '/api/plugins/holographic';
const MEMORY_OVERVIEW = `${MEMORY_BASE}/`;
const DELIVERY_OVERVIEW = '/api/delivery/overview';
const LOOM_TEMPORAL = '/api/loom/temporal';
const ANALYTICS_DIAGNOSTICS = '/api/plugins/analytics/diagnostics';
/** The donor envelope for routes the fixture registry does not answer. Any
 * envelope-shaped fixture would do; this one is checked by the endpoint
 * contract gate, so its scope, coverage, freshness and authorization are the
 * shapes `DashboardEnvelopeV1` actually requires. */
const ENVELOPE_DONOR = '/api/observatory';

/**
 * A contracted payload inside a real envelope.
 *
 * `DashboardEnvelopeV1` carries scope, version, time, watermark, coverage,
 * freshness, authorization and legal actions, and only `payload` varies by
 * route. A hand-written envelope missing one of those fails `DashboardEnvelopeV1Schema`,
 * and the surface then renders one generic schema notice for every state — so
 * the envelope is cloned from a fixture and only the payload is supplied here.
 */
function envelopeAround(payload: unknown): Record<string, unknown> {
  const base = structuredClone(resolveFixture(ENVELOPE_DONOR, '')) as Record<string, unknown>;
  if (typeof base['payload'] !== 'object' || base['payload'] === null) {
    throw new Error(`the ${ENVELOPE_DONOR} fixture is not envelope-shaped, so it cannot be a donor`);
  }
  base['payload'] = payload;
  return base;
}

/** A checked-in fixture with its payload edited in place. */
function editFixture(
  pathname: string,
  edit: (payload: Record<string, unknown>) => void,
): Record<string, unknown> {
  const base = structuredClone(resolveFixture(pathname, '')) as Record<string, unknown>;
  const payload = 'payload' in base ? base['payload'] : base;
  if (typeof payload !== 'object' || payload === null) {
    throw new Error(`the ${pathname} fixture carries no object to edit`);
  }
  edit(payload as Record<string, unknown>);
  return base;
}

/**
 * Every `Readout` on the page, keyed by its printed label.
 *
 * `ReadoutBar`'s own `label` becomes an `aria-label` on a plain div and never
 * reaches the screen, so the readings have to be read from the plates
 * themselves. `text` is the whole plate collapsed, which is where the note
 * under the figure lives — the note is usually the half that says whether a
 * dash means "measured nothing" or "could not read".
 */
async function readouts(page: Page): Promise<Record<string, { value: string; text: string }>> {
  return page.evaluate(() => {
    const out: Record<string, { value: string; text: string }> = {};
    for (const legend of Array.from(document.querySelectorAll('.td-legend'))) {
      const plate = legend.parentElement;
      const cell = plate?.querySelector('[data-cell="numeric"]');
      if (!plate || !cell) continue;
      const label = (legend.textContent ?? '').trim();
      if (label === '' || label in out) continue;
      out[label] = {
        value: (cell.textContent ?? '').trim(),
        text: (plate.textContent ?? '').replace(/\s+/g, ' ').trim(),
      };
    }
    return out;
  });
}

/* ==========================================================================
 * Settings — the effective-configuration count, and the review dialog.
 * ========================================================================== */

/**
 * The live count in the filter bar, parsed from whichever of its two forms is
 * on screen.
 *
 * This is the figure the trapped-pane defect hid behind: the bar went on
 * reporting a four-figure count while the pane holding those rows had resolved
 * to `height: 0`. The count is therefore checked against the rows that are
 * really rendered, and the pane's reachability is checked by the harness's
 * collapsed-scroller probe at 320 and 400% zoom.
 */
async function settingsCount(
  page: Page,
): Promise<{ shown: number; total: number; text: string }> {
  // No named helper inside `evaluate`: tsx compiles a named const arrow into an
  // `__name(...)` call that does not exist in the page, and the whole
  // assertion then fails as a ReferenceError rather than as a reading.
  return page.evaluate(() => {
    for (const node of Array.from(document.querySelectorAll('p'))) {
      const text = (node.textContent ?? '').trim();
      const both = /^(\d[\d,]*) of (\d[\d,]*) settings$/.exec(text);
      if (both) {
        return {
          shown: Number(both[1]!.replace(/,/g, '')),
          total: Number(both[2]!.replace(/,/g, '')),
          text,
        };
      }
      const one = /^(\d[\d,]*) settings$/.exec(text);
      if (one) {
        const count = Number(one[1]!.replace(/,/g, ''));
        return { shown: count, total: count, text };
      }
    }
    return { shown: -1, total: -1, text: '(no settings count is on the page)' };
  });
}

/** Leaf configuration rows actually rendered in the effective-configuration
 * pane. `countSettings` counts exactly the non-group rows, and `ValueRow` is
 * the only thing on this surface that emits a `dd`, so the two are directly
 * comparable. */
function renderedSettingRows(page: Page): Promise<number> {
  return page.locator('[aria-label="Effective configuration"] dd').count();
}

const SETTINGS_FILTER = 'Filter configuration';

/* ==========================================================================
 * Knowledge — a trust distribution whose finest source came back empty.
 * ========================================================================== */

/**
 * The overview with every trust bucket zeroed, which is what a real store
 * actually serves.
 *
 * `dashboard_compatibility_named_counts_tx` emits the bucket name as
 * `"trust-9"` and `facts.rs::trust_histogram` parses it with
 * `parse::<usize>()`, which fails and skips the row — so the ten-bucket
 * histogram is present, well-formed, and all zero. The buckets are kept
 * (rather than the array emptied) precisely because that is the shape: a
 * consumer that treats "present" as "populated" draws ten empty bars.
 */
function memoryWithZeroedHistogram(): Record<string, unknown> {
  return editFixture(MEMORY_OVERVIEW, (payload) => {
    const holographic = payload['holographic'] as Record<string, unknown>;
    const overview = holographic['overview'] as Record<string, unknown>;
    const histogram = overview['trust_histogram'] as Record<string, unknown>[];
    overview['trust_histogram'] = histogram.map((bucket) => ({ ...bucket, count: 0 }));
  });
}

/** The trust plate as rendered: its band rows and the caption naming the
 * source the counts came from. */
async function trustPlate(
  page: Page,
): Promise<{ bands: Array<{ label: string; count: string }>; caption: string }> {
  return page.evaluate(() => {
    const figures = Array.from(document.querySelectorAll('figure'));
    const plate = figures.find((figure) =>
      (figure.querySelector('figcaption')?.textContent ?? '').trim() === 'trust distribution',
    );
    if (!plate) return { bands: [], caption: '(no trust distribution plate on the page)' };
    const captions = Array.from(plate.querySelectorAll('figcaption'))
      .map((node) => (node.textContent ?? '').replace(/\s+/g, ' ').trim())
      .filter((text) => text !== 'trust distribution');
    const paragraphs = Array.from(plate.querySelectorAll('p')).map((node) =>
      (node.textContent ?? '').replace(/\s+/g, ' ').trim(),
    );
    const bands: Array<{ label: string; count: string }> = [];
    for (const row of Array.from(plate.querySelectorAll('div > div'))) {
      const cells = Array.from(row.querySelectorAll('[data-cell="numeric"]'));
      if (cells.length !== 2) continue;
      bands.push({
        label: (cells[0]!.textContent ?? '').trim(),
        count: (cells[1]!.textContent ?? '').trim(),
      });
    }
    return { bands, caption: [...captions, ...paragraphs].join(' | ') };
  });
}

/* ==========================================================================
 * Delivery — projections whose read authority is not mounted.
 * ========================================================================== */

/** An authority that answered, in the one shape the pipeline may print a
 * figure from. */
function deliveryChanges(): Record<string, unknown> {
  return {
    state: 'ready',
    value: {
      changed_paths: ['dashboard/e2e/axe-workspaces.ts', 'dashboard/e2e/axe-audit.ts'],
      conflicted: 0,
      head: { state: 'attached', branch: 'codex/dashboard-accessibility', commit: 'e7c1a90' },
      ignored: 4,
      operation: 'none',
      repository: '/fast/projects/tracedecay',
      schema_version: 'delivery-git-status-v1',
      staged: 2,
      unstaged: 0,
      untracked: 1,
    },
  };
}

function deliveryCommits(): Record<string, unknown> {
  return {
    state: 'ready',
    value: {
      truncated: true,
      items: [
        {
          author_at_micros: 1_785_000_000_000_000,
          author_email: 'zack@example.invalid',
          author_name: 'Zack',
          commit: '15ef9f578c0d4e2b',
          committer_at_micros: 1_785_000_000_000_000,
          subject: 'fix(dashboard): reach trapped panes and undersized controls',
        },
      ],
    },
  };
}

/** A projection whose read authority is absent. `reason` and
 * `required_authority` are both required by the contract, and both are what
 * the surface prints in place of a count. */
function absentAuthority(reason: string, authority: string): Record<string, unknown> {
  return { state: 'unavailable', reason, required_authority: authority };
}

function unsupportedSource(reason: string, authority: string): Record<string, unknown> {
  return { state: 'unsupported', reason, required_authority: authority };
}

/** Every pipeline stage's rendered chip, keyed by the stage's own label. */
async function pipelineStages(page: Page): Promise<Record<string, { state: string; text: string }>> {
  return page.evaluate(() => {
    const out: Record<string, { state: string; text: string }> = {};
    const labels = [
      'Changes & commits',
      'Pull requests & review',
      'Continuous integration',
      'Releases',
      'Index freshness',
    ];
    for (const stage of Array.from(document.querySelectorAll('div'))) {
      const head = stage.firstElementChild;
      if (head === null) continue;
      const label = (head.textContent ?? '').replace(/\s+/g, ' ').trim();
      if (!labels.includes(label) || label in out) continue;
      const chip = stage.querySelector('[data-state]');
      if (chip === null) continue;
      out[label] = {
        state: chip.getAttribute('data-state') ?? '',
        text: (stage.textContent ?? '').replace(/\s+/g, ' ').trim(),
      };
    }
    return out;
  });
}

/* ==========================================================================
 * Loom — time boundaries the store did and did not record.
 * ========================================================================== */

const LOOM_START = 1_784_900_000;

/** One session row, in `LoomSessionRowV1`. */
function loomSession(over: Record<string, unknown>): Record<string, unknown> {
  return {
    edited_files_recorded: false,
    ended_at: null,
    is_subagent: false,
    last_message_at: null,
    messages: 40,
    models: [{ model: 'gpt-5.6-terra' }],
    provider: 'codex',
    session_id: 'codex-boundary-0',
    started_at: LOOM_START,
    title: null,
    ...over,
  };
}

/**
 * Six sessions covering every extent boundary the surface distinguishes.
 *
 * The fourth is the one worth having: `ended_at` exactly equal to
 * `started_at` is the same instant recorded twice, not a duration, and a
 * surface that treated it as one would print a measured `0s` for a session
 * whose end nobody observed. The sixth has no start at all, so there is no
 * honest position for it on a time axis and it must be declared absent rather
 * than quietly dropped.
 */
function loomSessions(): Record<string, unknown>[] {
  return [
    loomSession({
      session_id: 'codex-recorded-end',
      title: 'recorded end',
      ended_at: LOOM_START + 5_400,
      last_message_at: LOOM_START + 5_100,
      messages: 212,
    }),
    loomSession({
      session_id: 'claude-last-message-only',
      title: 'last message only',
      provider: 'claude',
      started_at: LOOM_START + 7_200,
      last_message_at: LOOM_START + 10_800,
      messages: 96,
    }),
    loomSession({
      session_id: 'cursor-open-ended',
      title: 'no end observed',
      provider: 'cursor',
      started_at: LOOM_START + 14_400,
      messages: 33,
    }),
    loomSession({
      session_id: 'codex-instant-twice',
      title: 'end equals start',
      started_at: LOOM_START + 18_000,
      ended_at: LOOM_START + 18_000,
      messages: 7,
    }),
    loomSession({
      session_id: 'claude-hollow',
      title: 'zero messages',
      provider: 'claude',
      started_at: LOOM_START + 21_600,
      ended_at: LOOM_START + 23_400,
      messages: 0,
    }),
    loomSession({
      session_id: 'codex-undated',
      title: 'no start recorded',
      started_at: null,
      messages: 18,
    }),
  ];
}

function loomSourceStatus(over: Record<string, unknown>): Record<string, unknown> {
  return {
    authority: 'session_commit_attribution',
    coverage: {
      completeness: 'complete',
      eligible: 5,
      examined: 5,
      matched: 0,
      omitted: 5,
      reason: 'no commit overlapped the returned session page',
      unit: 'sessions',
    },
    granularity: 'per_session',
    id: 'session_commit',
    item_count: 0,
    label: 'Commit attribution',
    providers: ['codex', 'claude', 'cursor'],
    reason: null,
    required_authority: null,
    state: 'complete_zero_findings',
    ...over,
  };
}

function loomTemporalPayload(): Record<string, unknown> {
  return {
    available: true,
    branch_spans: [],
    commits: [],
    edited_files: [],
    sessions: loomSessions(),
    source_statuses: [
      loomSourceStatus({}),
      loomSourceStatus({
        id: 'branch_worktree',
        label: 'Branch and worktree spans',
        authority: 'branch_worktree_spans',
        item_count: null,
        state: 'unknown',
        reason: 'the branch/worktree span store could not be opened',
        coverage: {
          completeness: 'unknown',
          eligible: null,
          examined: null,
          matched: null,
          omitted: null,
          reason: 'no span rows were read, so nothing was counted',
          unit: null,
        },
      }),
      loomSourceStatus({
        id: 'delivery_outcomes',
        label: 'Delivery outcomes',
        authority: null,
        item_count: null,
        state: 'unsupported',
        reason: null,
        required_authority: 'delivery outcome projection is not mounted for this dashboard',
        coverage: {
          completeness: 'unknown',
          eligible: null,
          examined: null,
          matched: null,
          omitted: null,
          reason: 'the authority that would produce these rows is absent',
          unit: null,
        },
      }),
    ],
    temporal_refresh: {
      active_generations: 2,
      authority: 'daemon temporal generation registry',
      latest_activated_at_micros: (LOOM_START + 24_000) * 1_000_000,
      state: 'ready',
    },
    total: 41,
  };
}

/** The thread table's rows: one per session the weave placed. */
async function threadRows(
  page: Page,
): Promise<Array<{ session: string; extent: string }>> {
  return page.evaluate(() => {
    const table = document
      .querySelector('section[aria-label="Threads"]')
      ?.querySelector('table');
    if (!table) return [];
    return Array.from(table.querySelectorAll('tbody tr')).map((row) => {
      const cells = Array.from(row.querySelectorAll('td'));
      return {
        session: (cells[0]?.textContent ?? '').replace(/\s+/g, ' ').trim(),
        extent: (cells[4]?.textContent ?? '').replace(/\s+/g, ' ').trim(),
      };
    });
  });
}

/** The causal-crossings rail: each source status by its printed label, with the
 * state chip's own kind. */
async function causalSources(
  page: Page,
): Promise<Record<string, { state: string; text: string }>> {
  return page.evaluate(() => {
    const out: Record<string, { state: string; text: string }> = {};
    for (const legend of Array.from(document.querySelectorAll('.td-legend'))) {
      const row = legend.parentElement;
      const chip = row?.querySelector('[data-state]');
      if (!row || chip === null || chip === undefined) continue;
      const label = (legend.textContent ?? '').trim();
      if (label === '' || label in out) continue;
      out[label] = {
        state: chip.getAttribute('data-state') ?? '',
        text: (row.textContent ?? '').replace(/\s+/g, ' ').trim(),
      };
    }
    return out;
  });
}

/* ==========================================================================
 * Agents — an analytics read that failed, beside one that answered.
 * ========================================================================== */

export const WORKSPACE_SCENARIOS: readonly Scenario[] = [
  {
    id: 'settings-effective-configuration',
    route: '/settings',
    proves:
      'THE COUNT THE TRAPPED PANE HID BEHIND — the "N settings" claim equals the rows actually rendered, at every viewport and zoom',
    overrides: {},
    // The one new route on the matrix tier, and the reason is specific: the
    // collapsed-scroller site a hand sweep found here is at 400% zoom, a
    // combination only the matrix tier reaches. The showcase 320x568 row shares
    // the width but not the height, and this pane collapsed on height — so
    // without the matrix the gate would keep reporting /settings clean while
    // 4,388px of configuration sat behind a live count. `settings-canary`
    // proves this route renders something a scan can see; the extra
    // combinations lean on the five matrix canaries exercising the same
    // viewports, zooms and media modes in the same run.
    matrix: true,
    assert: async (page) => {
      // The pane is a scroll container, so it must take a tab stop and
      // announce a name: there is nothing inside it to tab to (WCAG 2.1.1),
      // and a tab stop that announces nothing is its own defect no axe rule
      // reports.
      const pane = page.locator('[aria-label="Effective configuration"]');
      if ((await pane.count()) !== 1) {
        throw new Error(
          `expected exactly one named effective-configuration pane, found ${await pane.count()}`,
        );
      }
      expectEqual((await pane.getAttribute('tabindex')) ?? '', '0', 'the pane tab stop');
      expectEqual((await pane.getAttribute('role')) ?? '', 'region', 'the pane role');

      // The claim, against the rows. A count that survives its own pane
      // collapsing is exactly what made the trapped configuration invisible,
      // so the figure is compared to what is really on the page rather than to
      // a literal written here.
      const unfiltered = await settingsCount(page);
      if (unfiltered.total < 1) {
        throw new Error(`the filter bar reported no configuration count: ${unfiltered.text}`);
      }
      const rows = await renderedSettingRows(page);
      if (rows !== unfiltered.total) {
        throw new Error(
          `FALSIFIED: the bar claims ${unfiltered.total} settings but the pane rendered ${rows} ` +
            `configuration rows (${unfiltered.text})`,
        );
      }
      await expectVisibleText(page, 'This API reports effective values only.', 'the provenance band');
      await expectVisibleText(page, 'in force', 'an environment override that is actually set');

      // Filtering is the other half of the same claim: "S of T" must describe
      // the rows the filter left behind, not the rows it started with.
      await page.getByLabel(SETTINGS_FILTER).fill('trace');
      await page.waitForTimeout(400);
      const filtered = await settingsCount(page);
      expectEqual(String(filtered.total), String(unfiltered.total), 'the denominator under a filter');
      if (filtered.shown >= filtered.total) {
        throw new Error(
          `the filter matched everything, so this assertion proves nothing: ${filtered.text}`,
        );
      }
      const filteredRows = await renderedSettingRows(page);
      if (filteredRows !== filtered.shown) {
        throw new Error(
          `FALSIFIED: the bar claims ${filtered.shown} of ${filtered.total} settings but the ` +
            `pane rendered ${filteredRows} rows`,
        );
      }
      // A filter that matches nothing says so, rather than leaving an empty
      // pane under a count.
      await page.getByLabel(SETTINGS_FILTER).fill('no-key-or-value-contains-this');
      await page.waitForTimeout(400);
      await expectVisibleText(page, 'no key or value matches', 'the empty-filter statement');
      expectEqual(String(await renderedSettingRows(page)), '0', 'rows behind an empty filter');
    },
  },
  {
    id: 'settings-review-dialog',
    route: '/settings',
    proves:
      'THE REVIEW PROMISE — the dialog sends only the field that changed, prints the revision it is held against, and will not apply unconfirmed',
    overrides: {},
    drive: async (page) => {
      // A change is required to reach the dialog at all: an unchanged plan
      // resolves to `unchanged` and reports "No project settings have
      // changed." instead of opening.
      await page.getByLabel('Maximum file size (bytes)').fill('2097152');
      await page.getByRole('button', { name: 'Review project changes' }).click();
      await page.waitForTimeout(500);
    },
    assert: async (page) => {
      const dialog = page.getByRole('dialog');
      if ((await dialog.count()) === 0) {
        throw new Error('the review dialog never opened, so nothing here was reviewed');
      }
      await expectVisibleText(page, 'Review project settings change', 'the dialog title');
      await expectVisibleText(page, 'expected revision rev-42', 'the held revision, printed');
      // "Only the validated changed fields below will be sent" is a claim, and
      // this is it measured: the patch is exactly the one field that moved.
      const patch = JSON.parse((await dialog.locator('pre').first().innerText()).trim()) as Record<
        string,
        unknown
      >;
      expectEqual(
        JSON.stringify(Object.keys(patch).sort()),
        JSON.stringify(['max_file_size']),
        'the patch carries only the changed field',
      );
      expectEqual(String(patch['max_file_size']), '2097152', 'the changed value');
      // Confirmation is a gate, not a decoration.
      const apply = dialog.getByRole('button', { name: 'Apply project settings' });
      expectEqual(String(await apply.isDisabled()), 'true', 'Apply before confirming');
      await dialog.getByRole('checkbox').first().check();
      expectEqual(String(await apply.isDisabled()), 'false', 'Apply after confirming');
      // A modal that does not hold focus is a modal a keyboard user is
      // standing outside of.
      const inside = await page.evaluate(() => {
        const active = document.activeElement;
        const modal = document.querySelector('[role="dialog"]');
        return active !== null && modal !== null && modal.contains(active);
      });
      if (!inside) {
        throw new Error('focus is outside the open review dialog, so it is not a modal in practice');
      }
    },
  },
  {
    id: 'knowledge-trust-histogram-empty',
    route: '/knowledge',
    proves:
      'THE DEGRADED READ — a trust histogram that came back all-zero falls through to the source that carries mass and names it, instead of drawing ten empty bars',
    overrides: {
      // Keyed on the prefix because the harness routes `**<key>**`, which also
      // catches `/status` and `/fact/{id}` under it. Only the overview is
      // replaced; the status route has to keep answering, since it is the
      // fallback under test.
      [MEMORY_OVERVIEW]: {
        status: 200,
        bodyFor: (url) =>
          url.pathname === MEMORY_OVERVIEW ||
          url.pathname === MEMORY_BASE ||
          url.pathname === `${MEMORY_BASE}/overview`
            ? memoryWithZeroedHistogram()
            : resolveFixture(url.pathname, url.search),
      },
    },
    assert: async (page) => {
      const plate = await trustPlate(page);
      if (plate.bands.length === 0) {
        throw new Error(`the trust distribution drew no bands at all: ${plate.caption}`);
      }
      // The fallback is the four bands the status route serves — the only
      // trust distribution a real store answers correctly — and the plate has
      // to say so, because the counts then cover the whole store rather than
      // the loaded slice.
      expectContains(
        plate.caption,
        'four bands the status route serves',
        'the plate names the source it actually read',
      );
      if (plate.caption.includes('no source reported a distribution')) {
        throw new Error(
          'FALSIFIED: a zeroed histogram beside a status route that answered read as no distribution at all',
        );
      }
      // Self-consistency, not a literal: the printed total is the sum of the
      // printed bands, and the occupancy claim is the count of bands that
      // carry anything.
      const counts = plate.bands.map((band) => Number(band.count.replace(/,/g, '')));
      if (counts.some((count) => !Number.isFinite(count))) {
        throw new Error(`a trust band printed a non-numeric count: ${JSON.stringify(plate.bands)}`);
      }
      const total = counts.reduce((sum, count) => sum + count, 0);
      const occupied = counts.filter((count) => count > 0).length;
      expectContains(
        plate.caption,
        `${total.toLocaleString()} facts across ${plate.bands.length} bands, ${occupied} of them occupied`,
        'the caption agrees with its own bands',
      );
      if (total === 0) {
        throw new Error('FALSIFIED: the fallback distribution is itself all zero, so it carries no mass');
      }
      // The fact list above the rows states what the loaded slice is, which is
      // the difference between "the store has no low-trust facts" and "you are
      // looking at the top of the list".
      await expectVisibleText(page, 'facts loaded', 'the loaded-slice header');
      const slice = await page.evaluate(() => {
        for (const legend of Array.from(document.querySelectorAll('.td-legend'))) {
          if (!(legend.textContent ?? '').includes('facts loaded')) continue;
          return (legend.parentElement?.textContent ?? '').replace(/\s+/g, ' ').trim();
        }
        return '';
      });
      if (!/(at trust \d\.\d\d|Trust \d\.\d\d–\d\.\d\d)/.test(slice)) {
        throw new Error(`the loaded slice printed no trust reading of its own: ${slice}`);
      }
    },
  },
  {
    id: 'delivery-pipeline-authorities-absent',
    route: '/delivery',
    proves:
      'THE TYPED UNAVAILABLE — a projection whose read authority is absent prints its reason, and never a zero count',
    overrides: {
      [DELIVERY_OVERVIEW]: {
        status: 200,
        body: envelopeAround({
          changes: deliveryChanges(),
          commits: deliveryCommits(),
          pull_requests: absentAuthority(
            'no GitHub read authority is mounted for this dashboard',
            'github_pull_request_reader',
          ),
          review_comments: absentAuthority(
            'no GitHub read authority is mounted for this dashboard',
            'github_review_comment_reader',
          ),
          ci_checks: unsupportedSource(
            'this repository has no CI provider TraceDecay can read',
            'ci_check_reader',
          ),
          failure_localization: unsupportedSource(
            'failure localization needs a CI provider first',
            'ci_failure_localizer',
          ),
          releases: absentAuthority(
            'the release authority has not been mounted',
            'release_reader',
          ),
          generation_freshness: absentAuthority(
            'no sealed generation is attached to this checkout',
            'code_index_generation_registry',
          ),
        }),
      },
    },
    assert: async (page) => {
      const stages = await pipelineStages(page);
      const missing = [
        'Changes & commits',
        'Pull requests & review',
        'Continuous integration',
        'Releases',
        'Index freshness',
      ].filter((label) => !(label in stages));
      if (missing.length > 0) {
        throw new Error(
          `the pipeline rendered no chip for ${missing.join(', ')} — found ${JSON.stringify(Object.keys(stages))}`,
        );
      }
      // The authority that answered is still reported in full, so an absent
      // neighbour never suppresses a live read.
      expectEqual(stages['Changes & commits']!.state, 'ready', 'the live Git read');
      expectContains(stages['Changes & commits']!.text, '1 commit', 'the commit count');
      expectContains(stages['Changes & commits']!.text, '2 changed paths', 'the changed-path count');

      expectEqual(stages['Pull requests & review']!.state, 'unknown', 'an absent read authority');
      expectContains(
        stages['Pull requests & review']!.text,
        'no GitHub read authority is mounted',
        'the absent authority’s own reason',
      );
      expectEqual(stages['Continuous integration']!.state, 'unsupported', 'an unsupported source');
      expectContains(
        stages['Continuous integration']!.text,
        'no CI provider TraceDecay can read',
        'the unsupported source’s reason',
      );
      expectEqual(stages['Releases']!.state, 'unknown', 'an unmounted release authority');
      expectEqual(stages['Index freshness']!.state, 'unknown', 'an unsealed generation');

      // The whole defect class, asserted directly: none of the four absent
      // projections may print a count.
      for (const label of [
        'Pull requests & review',
        'Continuous integration',
        'Releases',
        'Index freshness',
      ]) {
        const text = stages[label]!.text;
        if (/\b0 (pull requests|review comments|checks|releases|localized failures)\b/.test(text)) {
          throw new Error(
            `FALSIFIED: ${label} turned an unavailable projection into a measured zero: ${text}`,
          );
        }
      }
      // The registry field itself still answered, and the axis says which
      // recency it is drawing — the one word on this page that is routinely
      // misread.
      await expectVisibleText(page, 'not when it was last committed to', 'the index-recency caveat');
    },
  },
  {
    id: 'loom-extent-boundaries',
    route: '/loom',
    proves:
      'THE TIME BOUNDARIES — an unobserved end stays unrecorded rather than becoming a zero duration, and a session with no start is declared absent from the axis',
    overrides: {
      [LOOM_TEMPORAL]: { status: 200, body: envelopeAround(loomTemporalPayload()) },
    },
    assert: async (page) => {
      const rows = await threadRows(page);
      if (rows.length === 0) {
        throw new Error('the thread table rendered no rows, so no extent was measured at all');
      }
      // A session with no usable start has no honest position on a time axis,
      // so it is dropped from the field — and the surface has to say it was.
      if (rows.some((row) => row.session.includes('no start recorded'))) {
        throw new Error(
          'FALSIFIED: a session with no recorded start was placed on the time axis anyway',
        );
      }
      await expectVisibleText(
        page,
        'carried no usable start time',
        'the undated row, declared rather than silently dropped',
      );

      // `measured extent` is a fraction over the threads that ARE placed, and
      // it must agree with the table beside it rather than with a literal.
      const measured = rows.filter((row) => row.extent !== 'unrecorded');
      const plates = await readouts(page);
      const extent = plates['measured extent'];
      if (extent === undefined) {
        throw new Error(
          `no measured-extent readout on the page; readouts: ${JSON.stringify(Object.keys(plates))}`,
        );
      }
      expectEqual(
        extent.value,
        `${measured.length}/${rows.length}`,
        'the measured-extent fraction agrees with the thread table',
      );
      if (measured.length === rows.length || measured.length === 0) {
        throw new Error(
          `every thread has the same extent state (${measured.length}/${rows.length}), so this ` +
            'assertion cannot tell a measured end from an unmeasured one',
        );
      }
      // The boundary case this scenario exists for: an end recorded as the
      // same instant as the start is not a duration, and must not print as one.
      const instant = rows.find((row) => row.session.includes('end equals start'));
      if (instant === undefined) {
        throw new Error('the end-equals-start session never reached the thread table');
      }
      expectEqual(instant.extent, 'unrecorded', 'an end equal to the start');
      const openEnded = rows.length - measured.length;
      await expectVisibleText(
        page,
        `${openEnded} of ${rows.length} sessions have no recorded end`,
        'the axis statement agrees with the table',
      );
      // A session the store reports at zero messages is a reading, so it is
      // drawn and stated rather than treated as a gap.
      await expectVisibleText(page, 'at zero messages — a reading, not a gap', 'the hollow thread');
      // The causal rail's own version of the same distinction, and the one
      // worth pinning: a source that looked and found nothing prints its zero,
      // and a source whose authority is absent prints no count at all.
      const sources = await causalSources(page);
      const found = sources['Commit attribution'];
      const unread = sources['Branch and worktree spans'];
      const unmounted = sources['Delivery outcomes'];
      if (found === undefined || unread === undefined || unmounted === undefined) {
        throw new Error(
          `the causal rail rendered ${JSON.stringify(Object.keys(sources))} rather than all three source statuses`,
        );
      }
      expectEqual(found.state, 'complete_zero_findings', 'a source that examined and found nothing');
      expectContains(found.text, '0 rows', 'a measured zero, printed as a count');
      expectEqual(unread.state, 'unknown', 'a store that could not be opened');
      expectEqual(unmounted.state, 'unsupported', 'an authority that is not mounted');
      expectContains(
        unmounted.text,
        'delivery outcome projection is not mounted',
        'the unmounted authority names itself',
      );
      for (const [label, source] of [
        ['Branch and worktree spans', unread],
        ['Delivery outcomes', unmounted],
      ] as const) {
        if (/\d+ rows/.test(source.text)) {
          throw new Error(
            `FALSIFIED: ${label} could not be counted yet printed a row count: ${source.text}`,
          );
        }
      }
    },
  },
  {
    id: 'agents-diagnostics-unreadable',
    route: '/agents',
    proves:
      'THE SPLIT READ — analytics diagnostics that could not be read print no figure, and do not suppress the usage read that answered',
    overrides: {
      // `available: false` is what `analytics_api.rs` answers when the hook
      // analytics store cannot be folded. Every plate behind it has to say so;
      // the usage read on the sibling route is untouched.
      [ANALYTICS_DIAGNOSTICS]: { status: 200, body: { available: false } },
    },
    assert: async (page) => {
      const plates = await readouts(page);
      for (const label of ['mcp tool calls', 'hook calls']) {
        const plate = plates[label];
        if (plate === undefined) {
          throw new Error(
            `no ${label} readout on the page; readouts: ${JSON.stringify(Object.keys(plates))}`,
          );
        }
        // The defect this guards: a provenance figure that could not be read
        // must not print as zero.
        expectEqual(
          plate.value,
          '—',
          `the ${label} figure behind an unreadable read, which a measured zero would falsify`,
        );
      }
      expectContains(
        plates['hook calls']!.text,
        'analytics diagnostics unavailable',
        'the hook-window note names the failure',
      );
      // Three plates depend on the failed read and each says so in its own
      // right, rather than one banner standing in for all of them.
      const unavailable = page.getByText('Analytics diagnostics unavailable');
      const notices = await unavailable.count();
      if (notices < 3) {
        throw new Error(
          `expected each diagnostics-backed plate to report the failed read, found ${notices}`,
        );
      }
      // The other half of the split: the usage read answered, so its window
      // and its composition are still fully rendered.
      const events = plates['events (capped)'] ?? plates['events'];
      if (events === undefined) {
        throw new Error(
          `no event-window readout survived the failed diagnostics read; readouts: ${JSON.stringify(Object.keys(plates))}`,
        );
      }
      if (events.value === '—') {
        throw new Error(
          'FALSIFIED: a failed diagnostics read erased the event count the usage route did serve',
        );
      }
      await expectVisibleText(page, 'of all categorized events', 'the composition plate survived');
      await expectVisibleText(page, 'analytics_events', 'the window still names its source');
    },
  },
];
