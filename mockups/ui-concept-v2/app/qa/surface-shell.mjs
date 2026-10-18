import assert from 'node:assert/strict';
import { chromium } from 'playwright';

const browser = await chromium.launch();
const base = process.env.BASE_URL ?? 'http://127.0.0.1:5195';
try {
  const page = await browser.newPage({ viewport: { width: 1586, height: 992 }, reducedMotion: 'reduce' });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(`${base}/?surface=explorer`);
  await page.locator('.sess-card').first().waitFor();
  const count = await page.locator('.sess-card').count();
  await page.getByRole('searchbox', { name: 'Query', exact: true }).fill('no-captured-session-matches-this');
  assert.equal(await page.locator('.sess-card').count(), 0);
  await page.getByRole('banner').getByRole('button', { name: 'Clear', exact: true }).click();
  assert.equal(await page.locator('.sess-card').count(), count);
  await page.getByRole('button', { name: 'Close inspector', exact: true }).click();
  assert.equal(await page.locator('.inspector').count(), 0);
  await page.locator('.sess-card').first().click();
  await page.getByRole('button', { name: 'Close inspector', exact: true }).waitFor();
  await page.keyboard.press('Escape');
  assert.equal(await page.locator('.inspector').count(), 0);
  assert.equal(await page.locator('.sess-card').first().evaluate(el => el === document.activeElement), true);
  await page.keyboard.press('Enter');
  await page.getByRole('button', { name: 'Close inspector', exact: true }).waitFor();
  for (const testCase of [
    {
      surface: 'automations', state: 'run-143003', selected: '[data-run-id="run-143003"]',
      fixtureTruth: /AUTHORED FIXTURE · SYNTHETIC DATA/,
      snapshotTruth: /SNAPSHOT · SCHEDULER DATA UNAVAILABLE/,
      snapshotStatus: /SCHEDULER\s+unavailable[\s\S]*RUNS\s+not exported/,
    },
    {
      surface: 'workflows', state: 'code-intake', selected: '.wf-reg-table tr[aria-selected="true"]',
      fixtureTruth: /AUTHORED FIXTURE · SYNTHETIC DATA/,
      snapshotTruth: /SNAPSHOT · WORKFLOW AUTHORITY UNAVAILABLE/,
      snapshotStatus: /DEFINITIONS\s+unavailable[\s\S]*RUNS\s+not exported/,
    },
  ]) {
    const { surface, state } = testCase;
    await page.goto(`${base}/?surface=${surface}&data=fixture&state=${state}`);
    await page.locator('.surface-status').waitFor();
    await page.reload();
    assert.equal(new URL(page.url()).searchParams.get('state'), state);
    assert.equal(new URL(page.url()).searchParams.get('data'), 'fixture');
    assert.match(await page.locator('.surface-status').innerText(), /DATA\s+authored example[\s\S]*DESIGN FIXTURES \/ LOCAL ONLY/);
    assert.match(await page.locator(`.${surface === 'automations' ? 'am' : 'wf'}-truth`).innerText(), testCase.fixtureTruth);
    assert.equal(await page.locator(testCase.selected).getAttribute('aria-selected'), 'true');

    await page.goto(`${base}/?surface=${surface}&data=snapshot&state=${state}`);
    await page.locator('.surface-status').waitFor();
    await page.reload();
    assert.equal(new URL(page.url()).searchParams.get('state'), state);
    assert.equal(new URL(page.url()).searchParams.get('data'), 'snapshot');
    const snapshotStatus = await page.locator('.surface-status').innerText();
    assert.match(snapshotStatus, /DATA\s+recorded snapshot[\s\S]*RECORDED SNAPSHOTS \/ READ ONLY/);
    assert.match(snapshotStatus, testCase.snapshotStatus);
    assert.match(await page.locator(`.${surface === 'automations' ? 'am' : 'wf'}-truth`).innerText(), testCase.snapshotTruth);
    assert.equal(await page.locator(testCase.selected).count(), 0);
  }
  for (const surface of ['brain', 'explorer', 'loom', 'sessions', 'agents', 'code', 'knowledge', 'delivery', 'automations', 'observatory', 'costs', 'settings', 'work', 'workflows']) {
    await page.goto(`${base}/?surface=${surface}&dim=2d`);
    await page.waitForFunction(name => document.title.toLowerCase().endsWith(name), surface);
    assert.ok((await page.locator('body').innerText()).length > 200);
  }
  assert.deepEqual(errors, []);
  console.log('PASS surface titles, local Explorer query, inspector close/reopen/focus, and fixture/snapshot selection behavior; no page errors');
} finally {
  await browser.close();
}
