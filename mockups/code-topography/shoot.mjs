/**
 * Screenshot harness for the code-topography mockups.
 *
 * Playwright is not a dependency of this folder — it is resolved out of the
 * dashboard's node_modules, which is the only place in the repo that has it.
 * The location is read from an environment variable so no machine-local path
 * is ever committed:
 *
 *   TD_DASHBOARD_DIR   directory holding the dashboard's node_modules.
 *                      Default: ../../dashboard, relative to this script.
 *
 *   node shoot.mjs
 *   TD_DASHBOARD_DIR=/somewhere/else/dashboard node shoot.mjs
 *
 * Shoots every page at 1440 wide in both themes. Pages render deterministically
 * (seeded geometry, no animation), so re-running produces identical files.
 */
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';
import fs from 'node:fs';

const here = path.dirname(fileURLToPath(import.meta.url));
const dashboardDir = path.resolve(here, process.env.TD_DASHBOARD_DIR ?? '../../dashboard');
const requireFromDashboard = createRequire(path.join(dashboardDir, 'package.json'));

let chromium;
try {
  ({ chromium } = requireFromDashboard('playwright'));
} catch (cause) {
  console.error(
    `Could not load playwright from ${dashboardDir}.\n` +
      'Set TD_DASHBOARD_DIR to a directory whose node_modules contains playwright.',
  );
  throw cause;
}

const PAGES = ['cortex', 'trace', 'core-sample', 'lens'];
const THEMES = ['dark', 'light'];
const shots = path.join(here, 'shots');
fs.mkdirSync(shots, { recursive: true });

const browser = await chromium.launch();
const context = await browser.newContext({
  viewport: { width: 1440, height: 1200 },
  deviceScaleFactor: 2,
  reducedMotion: 'reduce',
});
const page = await context.newPage();
const problems = [];
page.on('pageerror', (error) => problems.push(String(error)));
page.on('console', (message) => {
  if (message.type() === 'error') problems.push(message.text());
});

for (const name of PAGES) {
  const file = path.join(here, `${name}.html`);
  if (!fs.existsSync(file)) continue;
  for (const theme of THEMES) {
    await page.goto(pathToFileURL(file).href, { waitUntil: 'load' });
    await page.evaluate((value) => {
      document.documentElement.setAttribute('data-theme', value);
    }, theme);
    await page.waitForTimeout(180);
    const out = path.join(shots, `${name}-${theme}.png`);
    await page.screenshot({ path: out, fullPage: true });
    console.log(`${path.relative(here, out)}`);
  }
}

await browser.close();
if (problems.length) {
  console.error('\npage errors:\n' + problems.join('\n'));
  process.exitCode = 1;
}
