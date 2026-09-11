import assert from 'node:assert/strict';
import { chromium } from 'playwright';

const base = process.env.BASE_URL ?? 'http://127.0.0.1:5195';
const browser = await chromium.launch();
try {
  const page = await browser.newPage({ viewport: { width: 1586, height: 992 }, reducedMotion: 'reduce' });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => sessionStorage.setItem('td:view:fixture:code.semantic-camera:cortex', JSON.stringify({ x: 0, y: 0, scale: 0 })));

  await page.goto(`${base}/?surface=explorer&data=snapshot`);
  await page.locator('.sess-card').first().waitFor();
  const capturedSessions = await page.locator('.sess-card').count();
  await page.getByRole('searchbox', { name: 'Query', exact: true }).fill('no-captured-session-matches-this');
  assert.equal(await page.locator('.sess-card').count(), 0, 'Explorer query filters the ready Sessions source');
  await page.getByRole('banner').getByRole('button', { name: 'Clear', exact: true }).click();
  assert.equal(await page.locator('.sess-card').count(), capturedSessions);
  assert.deepEqual(await page.locator('.sess-card .row-rel').allTextContents(), ['#1', '#2', '#3', '#4', '#5'], 'Session order is ordinal');

  await page.goto(`${base}/?surface=code&data=fixture&lens=cortex&symbol=focus`);
  assert.match(await page.getByLabel('cortex semantic lens').innerText(), /AUTHORED DESIGN FIXTURE · 26 SYMBOLS/);
  assert.match(await page.getByLabel('Semantic selection inspector').innerText(), /resolve_context/);
  await page.getByLabel('Search Code field').fill('no-authored-symbol-matches-this');
  await page.getByRole('button', { name: 'FIND', exact: true }).click();
  assert.equal(new URL(page.url()).searchParams.get('q'), 'no-authored-symbol-matches-this');
  await page.getByRole('button', { name: 'Clear Code search', exact: true }).click();
  assert.equal(new URL(page.url()).searchParams.has('q'), false, 'Clearing Code search deletes its URL authority');
  await page.reload();
  assert.equal(await page.getByLabel('Search Code field').inputValue(), '', 'A cleared Code query stays cleared after reload');
  let mainCanvas = await page.getByLabel(/CORTEX semantic field/).boundingBox();
  let miniCanvas = await page.getByLabel('CORTEX minimap').boundingBox();
  assert.ok(mainCanvas && miniCanvas && mainCanvas.width > 600 && miniCanvas.width <= 200 && miniCanvas.height <= 150, 'Minimap remains a distinct target beside the main field');
  assert.equal(await page.getByLabel('Semantic camera controls').locator('span').innerText(), '100%', 'An invalid persisted Code camera falls back to Fit');
  await page.getByLabel(/CORTEX semantic field/).focus();
  await page.getByLabel(/CORTEX semantic field/).press('Enter');
  assert.equal(await page.getByRole('tab', { name: 'TRACE', exact: true }).getAttribute('aria-selected'), 'true');
  assert.equal(new URL(page.url()).searchParams.get('symbol'), 'focus', 'Drill preserves semantic identity');
  assert.equal(await page.getByLabel('TRACE minimap').getAttribute('aria-label'), 'TRACE minimap');

  const traceCanvas = page.getByLabel(/TRACE semantic field/);
  const traceBox = await traceCanvas.boundingBox();
  assert.ok(traceBox);
  const fit = Math.min(traceBox.width / 1400, traceBox.height / 900) * .94;
  const point = (x, y) => ({ x: traceBox.x + traceBox.width / 2 + (x - 700) * fit, y: traceBox.y + traceBox.height / 2 + (y - 450) * fit });
  let target = point(205, 108);
  await page.mouse.move(target.x, target.y);
  assert.match(await page.getByLabel('Semantic selection inspector').innerText(), /HOVER PREVIEW[\s\S]*dispatch_tool_call/, 'Hover previews a distinct symbol');
  assert.equal(new URL(page.url()).searchParams.get('symbol'), 'focus', 'Hover does not move pinned identity');
  target = point(430, 470);
  await page.mouse.click(target.x, target.y);
  assert.equal(new URL(page.url()).searchParams.get('symbol'), 'sibcall', 'Click pins a Trace symbol');
  await page.getByRole('button', { name: 'Zoom semantic field in', exact: true }).click();
  assert.equal(await page.getByLabel('Semantic camera controls').locator('span').innerText(), '120%');
  await traceCanvas.focus();
  await traceCanvas.press('Enter');
  assert.equal(await page.getByRole('tab', { name: 'CORE', exact: true }).getAttribute('aria-selected'), 'true');
  const coreInspector = page.getByLabel('Core source inspector');
  assert.match(await coreInspector.innerText(), /hydrate_callable[\s\S]*312–368[\s\S]*Source text not included/);
  assert.match(await page.getByLabel(/CORE semantic field/).getAttribute('title'), /vendor_bridge\.rs/, 'Short Core headers retain the full source path as native text');
  assert.match(await coreInspector.innerText(), /INCOMING\s+1 caller · 5 sites[\s\S]*OUTGOING\s+1 callee · 16 sites/, 'Core distinguishes endpoint counts from authored call-site counts');
  assert.ok((await coreInspector.boundingBox())?.width >= 390, 'Core reserves a readable source and relationship panel');
  const coreReadoutBox = await page.locator('.cd-lens-readout').boundingBox();
  const coreControlsBox = await page.getByLabel('Semantic camera controls').boundingBox();
  assert.ok(coreReadoutBox && coreControlsBox && coreReadoutBox.x + coreReadoutBox.width <= coreControlsBox.x, 'Core status and camera controls occupy separate space');
  await coreInspector.getByRole('button', { name: 'SOURCE RANGE', exact: true }).click();
  assert.equal(new URL(page.url()).searchParams.get('core_view'), 'range', 'Core source-range zoom is URL-restorable');
  assert.equal(await coreInspector.getByRole('button', { name: 'SOURCE RANGE', exact: true }).getAttribute('aria-pressed'), 'true');
  await page.getByLabel('Semantic camera controls').getByRole('button', { name: 'BACK', exact: true }).click();
  assert.equal(new URL(page.url()).searchParams.has('core_view'), false, 'Back returns from source range to overview before leaving Core');
  assert.equal(await page.getByRole('tab', { name: 'CORE', exact: true }).getAttribute('aria-selected'), 'true');
  const coreSelect = coreInspector.getByLabel('Select Core symbol');
  await coreSelect.selectOption('core:crates/tracedecay-contracts/src/retrieval/service.rs:RetrievalService');
  await coreInspector.getByRole('button', { name: 'SOURCE RANGE', exact: true }).click();
  assert.match(await coreInspector.innerText(), /RetrievalService[\s\S]*34–58/, 'Source range clamps near the start of a file');
  await coreSelect.selectOption('core:crates/tracedecay-contracts/src/retrieval/service.rs:tests::budgets');
  assert.match(await coreInspector.innerText(), /tests::budgets[\s\S]*620–700/, 'Source range remains exact near the end of a file');
  await page.goto(`${base}/?surface=code&data=fixture&lens=core&symbol=focus`);
  const focusEvidence = await page.getByLabel('Core source inspector').innerText();
  assert.match(focusEvidence, /resolve_context[\s\S]*INCOMING\s+3 callers · 53 sites[\s\S]*OUTGOING\s+4 callees · 33 sites/, 'Highest-degree Core selection keeps exact authored endpoint and call-site counts');
  await page.goto(`${base}/?surface=code&data=fixture&lens=core&symbol=${encodeURIComponent('core:crates/tracedecay-contracts/src/retrieval/service.rs:RetrievalService')}`);
  const coreOnlyEvidence = await page.getByLabel('Core source inspector').innerText();
  assert.match(coreOnlyEvidence, /INCOMING\s+—[\s\S]*Not in the 26-symbol Trace sample[\s\S]*OUTGOING\s+—[\s\S]*Not in the 26-symbol Trace sample/, 'Core-only source bands do not fabricate zero Trace relations');
  await page.goto(`${base}/?surface=code&data=fixture&lens=core&symbol=sibcall`);
  await page.getByRole('button', { name: 'BACK', exact: true }).click();
  assert.equal(await page.getByRole('tab', { name: 'TRACE', exact: true }).getAttribute('aria-selected'), 'true');
  assert.equal(new URL(page.url()).searchParams.get('symbol'), 'sibcall', 'Back restores the prior lens with identity intact');
  assert.equal(await page.getByLabel('Semantic camera controls').locator('span').innerText(), '120%', 'Back restores the prior Trace camera');
  const restoredTrace = await page.getByLabel(/TRACE semantic field/).boundingBox();
  assert.ok(restoredTrace);
  const restoredFit = Math.min(restoredTrace.width / 1400, restoredTrace.height / 900) * .94 * 1.2;
  await page.mouse.dblclick(restoredTrace.x + restoredTrace.width / 2 + (430 - 700) * restoredFit, restoredTrace.y + restoredTrace.height / 2 + (470 - 450) * restoredFit);
  assert.equal(await page.getByRole('tab', { name: 'CORE', exact: true }).getAttribute('aria-selected'), 'true', 'Double-click drills to Core');
  await page.getByRole('button', { name: 'BACK', exact: true }).click();

  const exactPath = 'crates/tracedecay-store-runtime';
  await page.getByRole('tab', { name: 'EXACT FILES', exact: true }).click();
  let forwarding = page.locator('.repository-atlas .atlas-tools').nth(1).getByRole('button', { name: 'Forwarding', exact: true });
  await forwarding.click();
  assert.equal(await forwarding.getAttribute('aria-pressed'), 'true');
  await page.getByLabel('Find atlas path').fill(`${exactPath}/src`);
  await page.getByLabel('Matching atlas paths').getByRole('button', { name: new RegExp(`^${exactPath}/src · \\d+ files$`) }).click();
  const selectedSource = `${exactPath}/src`;
  assert.equal(await page.getByLabel('Atlas selected source').locator('h3').innerText(), selectedSource);
  await page.getByRole('tab', { name: 'CORTEX', exact: true }).click();
  await page.getByRole('tab', { name: 'EXACT FILES', exact: true }).click();
  forwarding = page.locator('.repository-atlas .atlas-tools').nth(1).getByRole('button', { name: 'Forwarding', exact: true });
  assert.equal(await page.getByLabel('Atlas selected source').locator('h3').innerText(), selectedSource);
  assert.equal(await forwarding.getAttribute('aria-pressed'), 'true', 'Semantic cameras do not reset the exact-files layer');

  await page.goto(`${base}/?surface=code&data=snapshot&lens=cortex`);
  assert.match(await page.getByLabel('cortex semantic lens').innerText(), /MEASURED GIT \+ CARGO SNAPSHOT/);
  assert.match(await page.getByLabel('cortex semantic lens').innerText(), /strata = Cargo depth/);
  assert.doesNotMatch(await page.getByLabel('cortex semantic lens').innerText(), /call sites/);
  await page.getByRole('tab', { name: 'TRACE', exact: true }).click();
  assert.match(await page.getByRole('status').innerText(), /No indexed symbols or call edges are attached/);
  await page.goto(`${base}/?surface=code&data=snapshot&node=${encodeURIComponent(exactPath)}&path=${encodeURIComponent(exactPath)}`);
  assert.equal(await page.getByRole('tab', { name: 'EXACT FILES', exact: true }).getAttribute('aria-selected'), 'true');
  assert.equal(await page.getByLabel('Atlas selected source').locator('h3').innerText(), exactPath, 'Explicit path arrival opens Exact files at the exact identity');

  await page.setViewportSize({ width: 793, height: 700 });
  const exactInspector = await page.getByLabel('Atlas selected source').boundingBox();
  const exactFooter = await page.locator('.cd-root .atlas-footer').boundingBox();
  assert.ok(exactInspector && exactFooter && exactInspector.y + exactInspector.height <= 700 && exactFooter.y + exactFooter.height <= 700, 'Narrow Exact files keeps its inspector and controls inside the visible pane');

  await page.setViewportSize({ width: 1263, height: 931 });
  await page.goto(`${base}/?surface=code&data=fixture&lens=core&symbol=assemble&core_view=range`);
  const exactFilesTab = await page.getByRole('tab', { name: 'EXACT FILES', exact: true }).boundingBox();
  const intermediateSearch = await page.getByLabel('Search Code field').locator('..').boundingBox();
  assert.ok(exactFilesTab && intermediateSearch && exactFilesTab.y + exactFilesTab.height <= intermediateSearch.y,
    'Intermediate-width Core reflows all four lenses above the search control without collision');

  await page.setViewportSize({ width: 793, height: 700 });
  await page.goto(`${base}/?surface=code&data=fixture&lens=trace&symbol=focus`);
  mainCanvas = await page.getByLabel(/TRACE semantic field/).boundingBox();
  miniCanvas = await page.getByLabel('TRACE minimap').boundingBox();
  assert.ok(mainCanvas && mainCanvas.height >= 500 && miniCanvas && miniCanvas.width <= 150 && miniCanvas.height <= 100, 'Narrow Trace keeps a navigable main field and separate minimap');
  assert.equal(await page.locator('.cd-root').evaluate(root => root.scrollWidth <= root.clientWidth), true, 'Narrow Code has no horizontal overflow');
  await page.setViewportSize({ width: 793, height: 496 });
  await page.getByRole('tab', { name: 'CORE', exact: true }).click();
  mainCanvas = await page.getByLabel(/CORE semantic field/).boundingBox();
  assert.ok(mainCanvas && mainCanvas.height >= 500, 'A 200%-equivalent viewport keeps Core scrollably navigable');
  assert.match(await page.getByLabel('Core source inspector').innerText(), /CODE[\s\S]*TEST[\s\S]*SOURCE COVERAGE UNAVAILABLE/, 'Core keeps its source-band legend available');
  const narrowRoot = page.locator('.cd-root');
  assert.equal(await narrowRoot.evaluate(root => root.scrollHeight > root.clientHeight), true, 'Narrow Code owns a real vertical scroll path');
  await narrowRoot.evaluate(root => { root.scrollTop = root.scrollHeight; });
  const narrowInspector = await page.getByLabel('Core source inspector').boundingBox();
  assert.ok(narrowInspector && narrowInspector.y < 496 && narrowInspector.y + narrowInspector.height > 0, 'Narrow scrolling reaches the readable Core inspector');
  const narrowRangeControl = await page.getByLabel('Core semantic zoom').getByRole('button', { name: 'SOURCE RANGE', exact: true }).boundingBox();
  assert.ok(narrowRangeControl && narrowRangeControl.y >= 0 && narrowRangeControl.y + narrowRangeControl.height <= 496, 'Narrow scrolling keeps Core semantic-zoom controls reachable');

  await page.goto(`${base}/?surface=knowledge&data=snapshot&node=${encodeURIComponent(exactPath)}`);
  assert.match(await page.locator('.kn-mode').innerText(), /SNAPSHOT · FACTS ABSENT/);
  assert.match(await page.locator('.kn-table').innerText(), /0 FACTS ATTACHED/);
  assert.doesNotMatch(await page.locator('.kn-root').innerText(), /ProjectStoreRuntimeV1 is the project store access owner/);
  await page.getByRole('tab', { name: 'GEOMETRY', exact: true }).click();
  assert.match(await page.locator('#kn-camera-panel').innerText(), /authority unavailable/i);

  await page.setViewportSize({ width: 1263, height: 992 });
  await page.goto(`${base}/?surface=knowledge&data=fixture`);
  assert.match(await page.locator('.kn-mode').innerText(), /AUTHORED EXAMPLE DATA/);
  const rows = page.locator('.kn-fixture-ledger tbody tr');
  assert.equal(await rows.count(), 3);
  await rows.nth(1).getByRole('button').click();
  const canonicalClaim = await rows.nth(1).getByRole('button').boundingBox();
  const canonicalSubject = await rows.nth(1).locator('td').nth(1).boundingBox();
  const authoredBadge = await page.locator('.kn-mode').boundingBox();
  const knowledgeControls = await page.locator('.kn-controls').boundingBox();
  assert.ok(canonicalClaim && canonicalSubject && canonicalClaim.x + canonicalClaim.width <= canonicalSubject.x,
    'Intermediate-width Knowledge wraps the canonical claim inside its ledger cell');
  assert.ok(authoredBadge && knowledgeControls && authoredBadge.x + authoredBadge.width <= knowledgeControls.x + knowledgeControls.width,
    'Intermediate-width Knowledge keeps the authored-example badge inside its controls');
  await rows.nth(0).getByRole('button').click();
  assert.equal(new URL(page.url()).searchParams.get('knowledge_fact'), 'runtime-v1-owner', 'Knowledge claim selection is URL-addressable');
  assert.equal(await page.locator('.kn-source').count(), 2);
  assert.match(await page.locator('.kn-related').innerText(), /superseded by/i);
  assert.match(await page.getByLabel('Authored example claim inspector').innerText(), /RETAINED CLAIM CONTENT/, 'Superseded evidence is labeled as retained claim content');
  await rows.nth(2).getByRole('button').click();
  assert.equal(new URL(page.url()).searchParams.get('knowledge_fact'), 'store-boundary-gone');
  assert.match(await page.locator('.kn-state.is-contradicted').first().innerText(), /contradicted/i);
  assert.match(await page.locator('.kn-related').innerText(), /contradicted by/i);
  await page.goBack();
  assert.match(await page.getByLabel('Authored example claim inspector').innerText(), /ProjectStoreRuntimeV1/, 'Back restores the prior Knowledge claim evidence');
  await page.reload();
  assert.match(await page.getByLabel('Authored example claim inspector').innerText(), /ProjectStoreRuntimeV1/, 'Reload restores Knowledge claim evidence from the URL');
  for (const camera of ['GEOMETRY', 'CURATION', 'OPLOG']) {
    await page.getByRole('tab', { name: camera, exact: true }).click();
    assert.match(await page.locator('#kn-camera-panel').innerText(), /AUTHORED EXAMPLE/);
  }

  await page.setViewportSize({ width: 793, height: 700 });
  await page.goto(`${base}/?surface=knowledge&data=fixture&knowledge_camera=facts`);
  for (const camera of ['FACTS', 'GEOMETRY', 'CURATION', 'OPLOG']) {
    const tab = await page.getByRole('tab', { name: camera, exact: true }).boundingBox();
    assert.ok(tab && tab.x >= 0 && tab.x + tab.width <= 793, `Narrow Knowledge keeps ${camera} reachable`);
  }
  assert.ok(await page.locator('.kn-mode').isVisible(), 'Narrow Knowledge keeps its source badge visible');
  assert.equal(await page.locator('.kn-root').evaluate(root => root.scrollWidth <= root.clientWidth), true, 'Narrow Knowledge has no horizontal overflow');

  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
  assert.deepEqual(errors, []);
  console.log('PASS Explorer query, Code continuum/source truth/atlas continuity/responsive fields, and Knowledge claim states');
} finally {
  await browser.close();
}
