/** Fast iteration shots: explorer + sessions only, against a running dev server. */
import { mkdirSync } from 'node:fs';
import path from 'node:path';
import { chromium } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { installApiFixtures } from './fixtures/route.ts';

const OUT = process.env['SHOT_OUT'] ?? '/tmp/shots';
const PORT = Number(process.env['AUDIT_PORT'] ?? 5399);
const BASE = `http://localhost:${PORT}`;
const SURFACES = (process.env['SHOT_SURFACES'] ?? 'explorer,sessions').split(',');
const WIDTHS = (process.env['SHOT_WIDTHS'] ?? '1440').split(',').map(Number);
const THEMES = (process.env['SHOT_THEMES'] ?? 'dark').split(',');
const QUERY = process.env['SHOT_QUERY'] ?? '';

function sleep(ms: number) {
  return new Promise((r) => setTimeout(r, ms));
}

async function main() {
  mkdirSync(OUT, { recursive: true });
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ deviceScaleFactor: 1 });
  const page = await context.newPage();
  await installApiFixtures(page);
  page.on('console', (m) => {
    if (m.type() === 'error') console.log('[console.error]', m.text());
  });
  page.on('pageerror', (e) => console.log('[pageerror]', String(e)));
  await page.goto(BASE, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('nav[aria-label="Workspaces"]', { timeout: 30_000 });
  for (const width of WIDTHS) {
    await page.setViewportSize({ width, height: 900 });
    for (const theme of THEMES) {
      for (const surface of SURFACES) {
        await page.goto(`${BASE}/${surface}`, { waitUntil: 'domcontentloaded' });
        await page.evaluate((t) => {
          localStorage.setItem('td-theme', t);
          document.documentElement.dataset['theme'] = t;
        }, theme);
        await page.waitForSelector('main#td-main', { timeout: 15_000 });
        await sleep(900);
        let suffix = '';
        if (QUERY !== '') {
          const box = page.locator('input[type="text"], input:not([type])').first();
          if ((await box.count()) > 0) {
            await box.fill(QUERY);
            await box.press('Enter');
            await sleep(1200);
            suffix = '__q';
          }
        }
        const file = `${surface}__${theme}__${width}${suffix}.png`;
        await page.screenshot({ path: path.join(OUT, file), fullPage: true });
        const axe = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze();
        console.log(
          `${file}  axe=${axe.violations.length} ${axe.violations.map((v) => v.id).join(',')}`,
        );
        for (const v of axe.violations) {
          console.log('   ', v.id, v.nodes.slice(0, 2).map((n) => n.html.slice(0, 160)));
        }
      }
    }
  }
  await browser.close();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
