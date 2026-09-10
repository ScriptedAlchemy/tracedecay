// Run from any directory after installing the dashboard's existing dependencies:
// node mockups/ui-concept-v2/03-loom/final/08-work-proximity.check.mjs
import assert from 'node:assert/strict';
import { chromium } from '../../../../dashboard/node_modules/playwright/index.mjs';

const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1680, height: 1050 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(new URL('./08-work-proximity.html', import.meta.url).href);
  assert.equal(await page.locator('nav .nav').count(), 14);
  assert.equal(await page.locator('.agentPath').count(), 32);

  const marker = page.locator('[data-encounter-id="overlap"]');
  await marker.click();
  assert.equal(await page.locator('.detail.show').count(), 1);
  await page.keyboard.press('Escape');
  assert.equal(await marker.evaluate(node => node === document.activeElement), true);
  await page.keyboard.press('Enter');
  assert.match(await page.locator('#factsA').innerText(), /edit_evt_cdx_422/);

  // A backwards seek must revoke already-open detail and every future marker,
  // including the minimap and any layout bend that would disclose the encounter.
  await page.locator('#replay').fill('30');
  assert.equal(await page.locator('.detail.show').count(), 0);
  assert.equal(await page.locator('.notice:visible').count(), 0);
  assert.equal(await page.locator('[data-observation]:visible').count(), 0);
  assert.equal(await page.locator('.agentPath').evaluateAll(paths =>
    paths.some(path => path.getAttribute('d').includes('C'))), false);
  await page.locator('#follow').click();
  assert.equal(await page.locator('.notice:visible').count(), 2);
  assert.equal(await page.locator('.miniEncounter:visible').count(), 2);
  assert.equal(await page.locator('.agentPath').evaluateAll(paths =>
    paths.some(path => path.getAttribute('d').includes('C'))), true);

  await page.locator('[data-range="12"]').click();
  const before = Number(await page.locator('.miniBox').getAttribute('x'));
  await page.locator('.fieldShell').focus();
  await page.keyboard.press('ArrowRight');
  assert.ok(Number(await page.locator('.miniBox').getAttribute('x')) > before);
  assert.equal(Number(await page.locator('.miniBox').getAttribute('width')), 314);
  await page.locator('#fit').click();
  assert.equal(Number(await page.locator('.miniBox').getAttribute('width')), 942);

  await page.setViewportSize({ width: 840, height: 525 });
  assert.equal(await page.evaluate(() =>
    document.documentElement.scrollWidth <= innerWidth), true);
  assert.deepEqual(errors, []);
  console.log('Proximity journey passed: selection, replay privacy, minimap, keyboard and reflow.');
} finally {
  await browser.close();
}
