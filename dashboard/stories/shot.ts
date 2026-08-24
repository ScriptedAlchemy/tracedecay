/**
 * Single-surface screenshot, for design iteration against a live daemon.
 *
 * `visual:audit` shoots all 72 fixture surfaces and is the ship gate; this is
 * the fast loop when you are iterating on one surface's appearance:
 *   npx tsx stories/shot.ts http://127.0.0.1:5199/brain out.png
 */
import { chromium } from '@playwright/test';

const url = process.argv[2] ?? 'http://127.0.0.1:5199/brain';
const out = process.argv[3] ?? 'shot.png';
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
await page.goto(url, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('main#td-main', { timeout: 15_000 }).catch(() => {});
await page.waitForTimeout(2_500);
await page.screenshot({ path: out });
console.log('wrote', out);
await browser.close();
