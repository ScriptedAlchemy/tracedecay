/**
 * Live render sweep: loads every workspace against a REAL daemon and fails if
 * any renders empty or falls through to React Router's error boundary.
 *
 * This is the complement to `visual:audit`, which runs against MSW fixtures
 * and so cannot catch a surface that only breaks on real payload shapes. It is
 * how the WebGL crash was caught: Brain and Code rendered fine under fixtures
 * while dying on any browser without a WebGL context.
 *
 * Point it at a dev server proxying a daemon, or at the daemon itself:
 *   TRACEDECAY_DASHBOARD_API=http://127.0.0.1:8397 npx rsbuild dev --port 5199
 *   SWEEP_BASE_URL=http://127.0.0.1:5199 npm run live:sweep
 */
import { chromium } from '@playwright/test';

const BASE = process.env['SWEEP_BASE_URL'] ?? 'http://127.0.0.1:5199';
const ROUTES =['brain','explorer','loom','sessions','agents','code','knowledge','delivery','automations','observatory','costs','settings'];
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();
const errors: string[] = [];
page.on('pageerror', (e) => errors.push(String(e.message).slice(0, 120)));
let bad = 0;
for (const route of ROUTES) {
  errors.length = 0;
  await page.goto(`${BASE}/${route}`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('main#td-main', { timeout: 15_000 }).catch(() => {});
  await page.waitForTimeout(1200);
  const text = (await page.locator('main#td-main').innerText().catch(() => '')) || '';
  // React Router's default boundary renders "Unexpected Application Error".
  const boundary = /Unexpected Application Error|blendFunc/i.test(text);
  const empty = text.trim().length === 0;
  const status = boundary ? 'ERROR-BOUNDARY' : empty ? 'EMPTY' : 'ok';
  if (status !== 'ok') bad++;
  console.log(
    `${route.padEnd(12)} ${status.padEnd(15)} chars=${String(text.length).padStart(5)} pageerrors=${errors.length}${errors.length ? ' :: ' + errors[0] : ''}`,
  );
}
console.log(bad === 0 ? 'LIVE SWEEP PASS: all 12 workspaces rendered against the daemon' : `LIVE SWEEP FAIL: ${bad} workspace(s)`);
await browser.close();
process.exitCode = bad === 0 ? 0 : 1;
