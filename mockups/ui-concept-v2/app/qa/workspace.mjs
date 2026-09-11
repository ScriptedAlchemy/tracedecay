import assert from 'node:assert/strict';
import { chromium } from 'playwright';

const base = process.env.BASE_URL ?? 'http://127.0.0.1:5195';
const browser = await chromium.launch();
try {
  const page = await browser.newPage({ viewport: { width: 1586, height: 992 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const attention = () => page.getByRole('button', { name: /^Attention: \d+ active sources$/ });
  const dialog = () => page.getByRole('dialog', { name: 'Attention / source evidence' });
  const marks = () => page.evaluate(() => JSON.parse(sessionStorage.getItem('td:view:snapshot:attention.marks') ?? '{}'));
  await page.goto(`${base}/?surface=delivery&data=snapshot&state=03&pr=web-infra-dev%2Frspack%2312977&branch=recorded-branch&attention=recorded-source`);
  await page.getByLabel('Data source', {exact:true}).selectOption('fixture');
  await page.waitForURL(url => url.searchParams.get('data') === 'fixture');
  assert.equal(new URL(page.url()).searchParams.get('state'), '03', 'View choice survives a source switch');
  for (const key of ['pr', 'branch', 'attention']) assert.equal(new URL(page.url()).searchParams.has(key), false, `${key} must not cross from recorded data into a fixture`);
  await page.getByLabel('Data source', {exact:true}).selectOption('snapshot');
  await page.waitForURL(url => url.searchParams.get('data') === 'snapshot');
  for (const key of ['pr', 'branch', 'attention']) assert.equal(new URL(page.url()).searchParams.has(key), false);
  await page.goto(`${base}/?surface=brain&data=snapshot`);
  // Snapshot Brain opens the recorded registry; atlas is an explicit
  // repository-structure view, not its default knowledge geometry.
  await page.getByRole('button', { name: 'Atlas / repository structure', exact: true }).click();
  await page.getByLabel('Measured repository atlas').waitFor();
  await attention().click();
  assert.equal(await dialog().locator('.attention-list button').filter({hasText:'follow-up candidate'}).count(), 0, 'Untracked GitHub results never enter shared attention');
  await page.getByLabel('Attention owner').selectOption('system');
  const first = dialog().locator('.attention-list button').filter({hasText:'Code index has no seal receipt'});
  await first.click();
  const sourceTitle = await dialog().locator('.attention-detail h3').innerText();
  const activeLabel = await attention().getAttribute('aria-label');
  await dialog().getByRole('button', { name: 'Acknowledge', exact: true }).click();
  assert.equal(await attention().getAttribute('aria-label'), activeLabel, 'Acknowledgment never resolves a source');
  assert.match(await dialog().locator('.attention-condition').innerText(), /^active/i);
  assert.ok(Object.values(await marks()).some(mark => mark.acknowledged && mark.seen));
  await dialog().getByRole('button', { name: 'Snooze until evidence changes', exact: true }).click();
  await page.getByLabel('Attention owner').selectOption('snoozed');
  assert.equal(await dialog().locator('.attention-list button').count(), 1);
  await dialog().getByRole('button', { name: 'Unsnooze', exact: true }).click();
  assert.equal(await dialog().locator('.attention-list button').count(), 0);
  await page.getByLabel('Attention owner').selectOption('system');
  await dialog().getByRole('button', { name: 'Show exact context ↗', exact: true }).click();
  await page.waitForURL(url => url.searchParams.get('surface') === 'observatory');
  assert.equal(new URL(page.url()).searchParams.get('observatory_finding'), 'pipeline');
  assert.equal(new URL(page.url()).searchParams.get('topology'), 'index');
  assert.equal(new URL(page.url()).searchParams.has('pr'), false, 'Coverage navigation never manufactures a PR identity');
  await attention().click();
  await page.getByLabel('Attention owner').selectOption('system');
  await dialog().locator('.attention-list button').filter({ hasText: sourceTitle }).click();
  assert.ok(await dialog().getByRole('button', { name: 'Unacknowledge', exact: true }).isVisible(), 'Marks survive a page pivot');
  await page.keyboard.press('Escape');
  assert.equal(await dialog().isVisible(), false);
  await page.goBack();
  await page.getByLabel('Measured repository atlas').waitFor();

  await page.getByLabel('Find atlas path').fill('crates/tracedecay-store-runtime');
  await page.getByLabel('Matching atlas paths').getByRole('button', { name: /^crates\/tracedecay-store-runtime · \d+ files$/ }).click();
  const selectedPath = await page.getByLabel('Atlas selected source').locator('h3').innerText();
  await page.getByRole('button', { name: 'Open same place in Code', exact: true }).click();
  await page.waitForURL(url => url.searchParams.get('surface') === 'code');
  assert.equal(await page.getByLabel('Atlas selected source').locator('h3').innerText(), selectedPath);
  assert.match(await page.getByRole('banner').innerText(), /CODE \/ EXACT FILES/, 'The shell identifies the mounted Code lens');
  await page.getByRole('button', { name: 'Open same place in Brain', exact: true }).click();
  await page.getByLabel('Measured repository atlas').waitFor();
  assert.equal(await page.getByLabel('Atlas selected source').locator('h3').innerText(), selectedPath);

  await page.evaluate(() => {
    const key = 'td:view:snapshot:attention.marks';
    const saved = JSON.parse(sessionStorage.getItem(key) ?? '{}');
    for (const mark of Object.values(saved)) mark.signature = 'earlier source evidence';
    sessionStorage.setItem(key, JSON.stringify(saved));
  });
  await page.reload();
  await attention().click();
  await page.getByLabel('Attention owner').selectOption('system');
  await dialog().locator('.attention-list button').filter({ hasText: sourceTitle }).click();
  assert.ok(await dialog().getByRole('button', { name: 'Acknowledge', exact: true }).isVisible(), 'New source evidence reopens old marks');
  await page.keyboard.press('Escape');

  await page.setViewportSize({ width: 793, height: 700 });
  await attention().click();
  assert.ok(await page.getByLabel('Attention data source').isVisible(), 'Compact rail retains the source switch');
  await page.getByLabel('Attention data source').selectOption('fixture');
  await page.waitForURL(url => url.searchParams.get('data') === 'fixture');
  await attention().click();
  assert.match(await dialog().innerText(), /Authored examples/);
  assert.doesNotMatch(await dialog().innerText(), /Provider metadata unchanged/);
  await dialog().locator('.attention-list button').filter({hasText: 'Fixture · Git conflict between worktrees'}).click();
  await dialog().getByRole('button', { name: 'Show exact context ↗', exact: true }).click();
  await page.locator('.proximity-detail').waitFor();
  assert.equal(new URL(page.url()).searchParams.get('loom_encounter'), 'fixture:proximity:git-conflict');
  assert.equal(new URL(page.url()).searchParams.get('loom_replay'), '1');
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);

  await page.setViewportSize({width:1586,height:992});
  const workBeacon = page.getByRole('button', {name:'13 Work',exact:true}).locator('.workspace-nav-beacon');
  assert.equal(await workBeacon.getAttribute('data-severity'), 'error', 'A failed Work task reaches the shared navigation');
  await attention().click();
  const blockedTask = dialog().locator('.attention-list button').filter({hasText:'blocked task: Project causal ledger'});
  await blockedTask.click();
  const countBeforeReplan = Number((await attention().getAttribute('aria-label')).match(/\d+/)[0]);
  await dialog().getByRole('button', {name:'Show exact context ↗',exact:true}).click();
  await page.waitForURL(url=>url.searchParams.get('work_task')==='T-204');
  await page.locator('[data-task-id="T-204"][aria-pressed=true]').waitFor();
  await page.getByRole('button', {name:/^Replan blocking edge/}).click();
  assert.equal(Number((await attention().getAttribute('aria-label')).match(/\d+/)[0]), countBeforeReplan-1, 'The shared beacon follows the canonical local task graph');
  await attention().click();
  assert.equal(await blockedTask.count(), 0, 'Replanned task is no longer represented as blocked');
  assert.equal(await dialog().locator('.attention-list button').filter({hasText:'failed task: Preserve failure provenance'}).count(), 1, 'Unrelated failure evidence remains active');
  await page.keyboard.press('Escape');
  assert.deepEqual(errors, []);
  console.log('PASS source isolation, persistent attention, exact pivots, native dialog, structural identity, live local Work conditions and compact navigation');
} finally {
  await browser.close();
}
