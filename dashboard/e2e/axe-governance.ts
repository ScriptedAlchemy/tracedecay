/**
 * Automations + Brain + nav-rail axe & screenshot harness.
 *
 * Scratch tool, modelled on `.explorer-axe/run.ts`: the shared
 * `stories/audit.ts` rm -rf's `audit-gallery/` and pins port 5173, and peer
 * agents are running it concurrently. This runs the same axe configuration
 * (wcag2a + wcag2aa + wcag21a + wcag21aa) against only the surfaces this lane
 * owns, on its own port and output directory, and drives each one through the
 * states a plain navigation never reaches:
 *
 *   the two review queues measured, and the two review queues unreadable;
 *   the global Doctor dot healthy, needing attention, and unreadable;
 *   the Brain registry field, and the Brain scoped to one project.
 *
 *   npx tsx .governance-axe/run.ts [label]
 */
import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { chromium, type Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { installApiFixtures } from '../stories/fixtures/route.ts';

const LABEL = process.argv[2] ?? 'current';
const PORT = Number(process.env['GOVERNANCE_AXE_PORT'] ?? 5747);
const OUT = path.join(process.cwd(), '.governance-axe', LABEL);
const WIDTHS = [320, 768, 1440] as const;
const THEMES = ['light', 'dark'] as const;

function envelope(payload: unknown, domainState: string): unknown {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'profile_sharded', store_root: '/data' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 10 },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
      eligible: 5,
      examined: 5,
      matched: 5,
      excluded: 0,
      omitted: 0,
      unknown: 0,
      denominator: 5,
      unit: 'producers',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: 10, watermark: null },
    domain_state: domainState,
    legal_actions: [],
    payload,
  };
}

const FINDING_KINDS = [
  'over_budget_store',
  'orphan_store',
  'stale_branch_dbs',
  'incident_debris_present',
  'retention_backlog',
] as const;

/** `/api/storage/findings` in the shape the nav rail actually parses
 * (`StorageFindingsPayloadSchema`: `{ kinds, note }`). */
function findingsPayload(states: readonly string[]): unknown {
  return envelope(
    {
      kinds: FINDING_KINDS.map((kind, index) => ({
        kind,
        state: states[index] ?? 'healthy_complete_coverage',
        required_source: 'doctor.storage',
        reason: 'harness fixture',
      })),
      note: 'harness fixture',
    },
    'ready',
  );
}

const FINDINGS_HEALTHY = findingsPayload(Array(5).fill('healthy_complete_coverage'));
const FINDINGS_ATTENTION = findingsPayload([
  'healthy_complete_coverage',
  'degraded',
  'healthy_complete_coverage',
  'healthy_complete_coverage',
  'healthy_complete_coverage',
]);

/** Scheduler status with both review queues measured. */
const SCHEDULER_MEASURED = {
  status: 'configured',
  paused: false,
  enabled: true,
  scheduler_tick_secs: 900,
  pending_fact_proposals: 5,
  pending_skills: 2,
  pending_review: {
    fact_proposals: { state: 'measured', count: 5, reason: null },
    skills: { state: 'measured', count: 2, reason: null },
  },
  now: 1_800_000_000,
  last_session_activity: 1_799_998_800,
  tasks: [],
};

/** The defect's own scenario: HTTP 200, and neither review queue could be
 * read. This must never render as `0`. */
const SCHEDULER_UNREADABLE = {
  status: 'configured',
  paused: false,
  enabled: true,
  scheduler_tick_secs: 900,
  pending_fact_proposals: null,
  pending_skills: null,
  pending_review: {
    fact_proposals: {
      state: 'unreadable',
      count: null,
      reason: 'the project fact authority could not be read: database is locked',
    },
    skills: {
      state: 'unreadable',
      count: null,
      reason: 'the user profile root could not be resolved: $HOME is not set',
    },
  },
  now: 1_800_000_000,
  last_session_activity: 1_799_998_800,
  tasks: [],
};

interface Scenario {
  readonly state: string;
  readonly route: string;
  readonly findings: unknown;
  readonly scheduler: unknown;
  /** Click a registry project row before scanning (drives the scoped Brain). */
  readonly scopeToProject?: boolean;
}

const SCENARIOS: readonly Scenario[] = [
  {
    state: 'automations-measured',
    route: '/automations',
    findings: FINDINGS_HEALTHY,
    scheduler: SCHEDULER_MEASURED,
  },
  {
    state: 'automations-unreadable',
    route: '/automations',
    findings: FINDINGS_ATTENTION,
    scheduler: SCHEDULER_UNREADABLE,
  },
  {
    // No `kinds` in the response the rail can parse: the read is broken, and
    // the dot has to say so rather than read as an all-clear.
    state: 'nav-unknown',
    route: '/automations',
    findings: envelope({ note: 'unparseable for the rail' }, 'error'),
    scheduler: SCHEDULER_MEASURED,
  },
  {
    state: 'brain-registry',
    route: '/brain',
    findings: FINDINGS_HEALTHY,
    scheduler: SCHEDULER_MEASURED,
  },
  {
    state: 'brain-scoped',
    route: '/brain',
    findings: FINDINGS_HEALTHY,
    scheduler: SCHEDULER_MEASURED,
    scopeToProject: true,
  },
];

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

/** What the surface is claiming right now, read back out of the DOM. This is
 * the falsification check: a review tile must never print a bare `0` while its
 * queue is unreadable, and the Doctor dot must not report health it does not
 * have. */
async function readClaims(page: Page): Promise<Record<string, string>> {
  // No inner functions in this body: tsx compiles the callback with esbuild's
  // `keepNames`, whose `__name` helper does not exist inside the page.
  return page.evaluate(() => {
    const wanted = ['pending proposals', 'pending skills'];
    const claims: Record<string, string> = {
      'pending proposals': 'ABSENT',
      'pending skills': 'ABSENT',
    };
    for (const legend of Array.from(document.querySelectorAll('.td-legend'))) {
      const label = (legend.textContent ?? '').trim();
      if (wanted.includes(label)) {
        claims[label] = (legend.parentElement?.textContent ?? '').trim();
      }
    }
    const dot = document.querySelector('[data-doctor-health]');
    claims['doctorHealth'] = dot?.getAttribute('data-doctor-health') ?? 'ABSENT';
    claims['doctorLabel'] = dot?.getAttribute('aria-label') ?? 'ABSENT';
    return claims;
  });
}

async function main(): Promise<void> {
  mkdirSync(OUT, { recursive: true });
  const server = spawn('npx', ['rsbuild', 'dev', '--port', String(PORT)], {
    cwd: process.cwd(),
    stdio: 'ignore',
    detached: true,
    env: { ...process.env, NO_COLOR: '1' },
  });
  const base = `http://127.0.0.1:${PORT}`;
  const deadline = Date.now() + 120_000;
  for (;;) {
    if (Date.now() > deadline) throw new Error('dev server did not start');
    try {
      const res = await fetch(base, { signal: AbortSignal.timeout(4_000) });
      if (res.ok) break;
    } catch {
      /* not up yet */
    }
    await new Promise((r) => setTimeout(r, 400));
  }

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ deviceScaleFactor: 1 });
  const page = await context.newPage();
  await installApiFixtures(page);

  let scenario: Scenario = SCENARIOS[0]!;
  // Registered after the generic fixture route so Playwright (last match wins)
  // prefers the per-scenario payloads.
  await page.route('**/api/storage/findings*', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(scenario.findings),
    });
  });
  await page.route('**/api/automation/scheduler/status*', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(scenario.scheduler),
    });
  });
  // Passed as source text rather than a function: tsx compiles callbacks with
  // esbuild's `keepNames`, whose `__name` helper does not exist in the page.
  await page.addInitScript({
    content: `document.addEventListener('DOMContentLoaded', function () {
      var style = document.createElement('style');
      style.textContent = '*,*::before,*::after{animation-duration:0s!important;animation-delay:0s!important;transition-duration:0s!important;transition-delay:0s!important;}';
      document.head.appendChild(style);
    });`,
  });

  const claims: Record<string, Record<string, string>> = {};
  let total = 0;
  for (const width of WIDTHS) {
    await page.setViewportSize({ width, height: 900 });
    for (const theme of THEMES) {
      for (const next of SCENARIOS) {
        scenario = next;
        console.log(`[axe] ${next.state} ${theme} @ ${width}`);
        await page.goto(`${base}${next.route}`, { waitUntil: 'domcontentloaded' });
        await setTheme(page, theme);
        await page.waitForSelector('main#td-main', { timeout: 20_000 });
        await page.waitForTimeout(1_100);

        if (next.scopeToProject) {
          const row = page.getByRole('button', { name: /tracedecay/i }).first();
          if ((await row.count()) > 0) {
            await row.click({ force: true });
            await page.waitForTimeout(1_100);
          }
        }

        total += await scan(page, next.state, theme, width);
        await page.screenshot({
          path: path.join(OUT, `${next.state}__${theme}__${width}.png`),
          fullPage: true,
        });
        claims[`${next.state}__${theme}__${width}`] = await readClaims(page);
      }
    }
  }

  writeFileSync(path.join(OUT, 'findings.json'), `${JSON.stringify(findings, null, 2)}\n`);
  writeFileSync(path.join(OUT, 'claims.json'), `${JSON.stringify(claims, null, 2)}\n`);

  const byRule = new Map<string, number>();
  for (const f of findings)
    for (const v of f.violations) byRule.set(v.id, (byRule.get(v.id) ?? 0) + 1);

  console.log(`\n===== governance axe (${LABEL}) =====`);
  for (const f of findings) {
    const tag = `${f.state}/${f.theme}/${f.width}`;
    console.log(`  ${tag.padEnd(38)} ${f.violations.length}`);
    for (const v of f.violations) {
      console.log(`      - ${v.id} [${v.impact}] x${v.nodes.length} — ${v.help}`);
      for (const n of v.nodes.slice(0, 3)) console.log(`          ${n}`);
    }
  }
  console.log(`\n  scans=${findings.length}  totalViolations=${total}`);
  console.log(`  byRule=${JSON.stringify(Object.fromEntries(byRule))}`);
  console.log(`  shots=${OUT}`);

  await browser.close();
  if (server.pid) {
    try {
      process.kill(-server.pid, 'SIGTERM');
    } catch {
      /* already gone */
    }
  }
  process.exit(0);
}

void main();
