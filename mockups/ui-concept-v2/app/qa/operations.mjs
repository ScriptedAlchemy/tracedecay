import assert from 'node:assert/strict';
import { chromium } from 'playwright';

const base = (process.env.BASE_URL ?? 'http://127.0.0.1:5195').replace(/\/$/, '');
const browser = await chromium.launch();

try {
  const page = await browser.newPage({ viewport: { width: 1586, height: 992 }, reducedMotion: 'reduce' });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error' && !message.text().includes('ResizeObserver loop')) errors.push(message.text());
  });

  await page.goto(`${base}/?surface=automations&data=fixture&state=run-143612`);
  await page.locator('.am-fleet').waitFor();
  const failedAttempt = page.locator('.am-trace-node.is-failed');
  const successfulRetry = page.locator('.am-trace-node.is-success');
  assert.equal(await failedAttempt.count(), 1, 'The durable failed attempt remains in the selected run lineage');
  assert.equal(await successfulRetry.count(), 1, 'The successful retry remains linked beside the failure');
  await failedAttempt.click();
  assert.equal(await failedAttempt.getAttribute('aria-pressed'), 'true');
  assert.equal(await successfulRetry.isVisible(), true, 'Selecting the failure must not rewrite or hide the retry');
  assert.match(await page.locator('.am-lineage').innerText(), /failed[\s\S]*success/);
  await page.locator('[data-run-id="run-133207"]').click();
  assert.match(await page.locator('.am-canvas').innerText(), /run-133207 · attempt lineage unserved/);
  assert.doesNotMatch(await page.locator('.am-canvas').innerText(), /run-143612/);
  assert.equal(await page.locator('.am-lineage').count(), 0, 'An older run never inherits the newest run lineage for the same job');

  await page.goto(`${base}/?surface=workflows&data=fixture&state=enrich-graph&run=run_wait_4d`);
  await page.locator('.wf-topology').waitFor();
  assert.match(await page.locator('.wf-track.definition').innerText(), /ACTIVE · pins matched/);
  assert.match(await page.locator('.wf-track.run').innerText(), /WAITING · no terminal receipt/);
  assert.match(await page.locator('.wf-track.delivery').innerText(), /NOT ACCEPTED[\s\S]*no proved deliverable join/);
  assert.equal(await page.locator('.wf-node.waiting').count(), 1);
  assert.equal(await page.locator('.wf-node.unexercised').count(), 2);
  await page.locator('.wf-track.delivery').click();
  await page.waitForURL(url => url.searchParams.get('surface') === 'delivery');
  const incoming = page.getByText(/Incoming workflow context:/).first();
  assert.match(await incoming.innerText(), /enrich-graph · run run_wait_4d · task task_td-418/);
  assert.match(await incoming.innerText(), /no PR or readiness join is established/);
  await page.getByText('Return to workflow', { exact: true }).click();
  await page.locator('.wf-track.run').waitFor();
  assert.equal(new URL(page.url()).searchParams.get('run'), 'run_wait_4d');
  assert.match(await page.locator('.wf-track.run').innerText(), /WAITING · no terminal receipt/);

  await page.goto(`${base}/?surface=observatory&data=snapshot`);
  await page.locator('[data-topology-node="index"]').waitFor();
  await page.locator('.ob-attention-item').filter({ hasText: 'Code index has no seal receipt' }).click();
  assert.equal(new URL(page.url()).searchParams.get('observatory_finding'), 'pipeline');
  assert.equal(new URL(page.url()).searchParams.get('topology'), 'index');
  assert.match(await page.locator('#ob-evidence').innerText(), /Code-index pipeline[\s\S]*SOURCE[\s\S]*tracedecay\.db/);
  await page.locator('[data-topology-node="retrieval"]').click();
  const retrieval = await page.locator('#ob-evidence').innerText();
  assert.match(retrieval, /Affected consumer · retrieval_anchors/);
  assert.match(retrieval, /measured empty/i);
  assert.match(retrieval, /unsealed index does not turn an empty anchor table into a failure/);
  assert.equal(await page.locator('.topo-edges path').count(), 2, 'Only the two declared causal/comparison relations render');

  assert.deepEqual(errors, []);
  console.log('PASS durable retry lineage and exact-run isolation, workflow recipe/run/delivery separation and return, Observatory source/affected causality');
} finally {
  await browser.close();
}
