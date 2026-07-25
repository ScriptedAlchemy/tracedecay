/**
 * Minimal file:// screenshooter for the structure-viz mockups.
 *
 * Deliberately NOT the product's stories/shot.ts: these are standalone concept
 * pages with no daemon, no dev server and no #td-main to wait for. Each page
 * reads ?theme=… itself, so one pass per theme is all this needs.
 *
 *   node mockups/structure-viz/shoot.mjs            # all pages, both themes
 *   node mockups/structure-viz/shoot.mjs anatomy    # one page
 */
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));

/* A git worktree has no node_modules of its own — the dashboard's install
 * lives in the primary checkout. Resolve Playwright from whichever dashboard
 * dir actually has it rather than requiring an install per worktree. */
function loadPlaywright() {
  const roots = [
    resolve(here, '../../dashboard'),
    process.env.TD_DASHBOARD_DIR,
    '/fast/projects/tracedecay/dashboard',
  ].filter(Boolean);
  for (const root of roots) {
    if (!existsSync(join(root, 'node_modules/@playwright/test'))) continue;
    return createRequire(join(root, 'package.json'))('@playwright/test');
  }
  throw new Error(
    'could not find @playwright/test; set TD_DASHBOARD_DIR to a dashboard dir that has node_modules',
  );
}
const { chromium } = loadPlaywright();
const PAGES = [
  { slug: 'symbol-anatomy', height: 1180 },
  { slug: 'call-chain-transit', height: 1000 },
  { slug: 'disagreement-field', height: 1080 },
];

const filter = process.argv[2];
const wanted = filter ? PAGES.filter((p) => p.slug.includes(filter)) : PAGES;
if (wanted.length === 0) throw new Error(`no mockup matches "${filter}"`);

const browser = await chromium.launch({ headless: true });
for (const { slug, height } of wanted) {
  for (const theme of ['dark', 'light']) {
    const page = await browser.newPage({
      viewport: { width: 1440, height },
      deviceScaleFactor: 2,
      // Freeze the one animated mark so consecutive shots are byte-comparable.
      reducedMotion: 'reduce',
    });
    const url = `${pathToFileURL(join(here, `${slug}.html`)).href}?theme=${theme}`;
    await page.goto(url, { waitUntil: 'load' });
    await page.waitForTimeout(400);
    const out = join(here, 'shots', `${slug}-${theme}.png`);
    await page.screenshot({ path: out, fullPage: true });
    console.log('wrote', out);
    await page.close();
  }
}
await browser.close();
