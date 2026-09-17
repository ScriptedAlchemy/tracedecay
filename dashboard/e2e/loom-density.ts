/** Exercise the shipped Loom temporal field with bounded, explicitly synthetic wire data.
 * Run from dashboard: AXE_PORT=5357 npx tsx e2e/loom-density.ts [output-directory]
 * No daemon/profile writes. Timings include browser input and two paint frames.
 */
import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { chromium } from '@playwright/test';
import { AnalyticsSubagentTreePayloadV1Schema, LcmSessionPayloadV1Schema, LoomTemporalPayloadV1Schema } from '../src/contracts/generated.ts';
import { resolveFixture } from '../stories/fixtures/data.ts';
import { installApiFixtures } from '../stories/fixtures/route.ts';
import { startStaticServer } from './static-server.ts';

const out = path.resolve(process.argv[2] ?? '.axe-audit/loom-density');
mkdirSync(out, { recursive: true });
const base = resolveFixture('/api/loom/temporal');
assert(base && typeof base === 'object' && 'payload' in base);
const temporal = LoomTemporalPayloadV1Schema.parse(base.payload);
assert(temporal.sessions[0]);
const start = 1_784_700_000;
const sessions = Array.from({ length: 100 }, (_, i) => ({
  ...temporal.sessions[0], session_id: `fixture-session-${i}`, provider: i < 50 ? 'claude' : 'codex',
  title: `Fixture agent ${i}`, started_at: start + i * 30,
  ended_at: i % 17 === 0 ? null : start + i * 30 + 900,
  last_message_at: null, messages: 200, is_subagent: i !== 0 && i !== 50,
}));
const parentIndex = (i: number): number | null => i === 0 || i === 50 || i === 17 || i === 89 ? null : (i < 50 ? 0 : 50) + Math.floor(((i % 50) - 1) / 2);
const ancestors = (i: number): number[] => { const parent = parentIndex(i); return parent == null ? [] : [parent, ...ancestors(parent)]; };
const preorder: number[] = [];
const append = (i: number) => { preorder.push(i); sessions.forEach((_, child) => { if (parentIndex(child) === i) append(child); }); };
sessions.forEach((_, i) => { if (parentIndex(i) == null) append(i); });
const hierarchy = AnalyticsSubagentTreePayloadV1Schema.parse({
  available: true, source: 'sessions', error: null, sessions_read: 100, root_count: 2,
  edge_count: 96, missing_parent_count: 2, cycle_count: 0, max_depth: 5, truncated: false,
  nodes: preorder.map((i) => ({
    provider: sessions[i]!.provider, session_id: sessions[i]!.session_id, title: sessions[i]!.title,
    agent: `fixture-agent-label-${i}`, parent_session_id: parentIndex(i) == null ? (i === 17 || i === 89 ? 'outside-loaded-page' : null) : sessions[parentIndex(i)!]!.session_id,
    parent_tool_use_id: parentIndex(i) == null ? null : `fixture-tool-${i}`,
    started_at: sessions[i]!.started_at, ended_at: sessions[i]!.ended_at,
    is_subagent: sessions[i]!.is_subagent, depth: ancestors(i).length,
    descendants: sessions.filter((_, child) => ancestors(child).includes(i)).length,
    link: i === 17 || i === 89 ? 'missing_parent' : parentIndex(i) == null ? 'root' : 'linked',
  })),
});
const loaded = LoomTemporalPayloadV1Schema.parse({ ...temporal, sessions, total: 100 });
const pageFor = (sessionId: string) => LcmSessionPayloadV1Schema.parse({
  exists: true, session_id: sessionId, path: '/fixture/sessions.db', storage_scope: 'fixture',
  limit: 200, next_cursor: null, has_more: false, has_more_messages: false, has_more_summary_nodes: false,
  counts: { message_count: 200, source_token_count: null, summary_node_count: 0, summary_token_count: null }, summary_nodes: [],
  messages: Array.from({ length: 200 }, (_, i) => ({
    session_id: sessionId, message_id: `event-${i}`, ordinal: i, timestamp: start + i * 10,
    content: `Explicit density fixture message ${i}.`, snippet: null, role: i % 3 === 0 ? 'user' : 'assistant',
    tool_name: i % 3 === 1 ? 'Read' : null, token_count: null, token_count_provenance: null,
    pinned: 0, source: sessions.find((session) => session.session_id === sessionId)?.provider ?? null,
    storage_kind: 'message', store_id: null, summary_node_ids: [], metadata_json: null,
  })),
});
const descendantsOf = (i: number) => hierarchy.nodes.find((node) => node.session_id === `fixture-session-${i}`)!.descendants;
// Four roots in the loaded page: two recorded roots and two whose recorded
// parent is outside the page. Above the dense threshold every root WITH
// descendants starts as a bundle; agent 89 has none and stays a plain lane.
const ROOTS = 4;
const BUNDLES = 3;
const transcriptNodes = (page: import('@playwright/test').Page) => page.locator('[data-event][data-kind^="message"], [data-event][data-kind="tool_call"]');

const { baseURL, server } = startStaticServer();
const browser = await chromium.launch({ headless: true });
const timings: Record<string, number> = {};
const errors: string[] = [];
let peakDomNodes = 0;
let previousLongTasks: number[] = [];
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 1 });
  page.on('pageerror', (error) => errors.push(error.message));
  await page.addInitScript(() => {
    const durations: number[] = [];
    Object.assign(window, { loomLongTasks: durations });
    new PerformanceObserver((entries) => { for (const entry of entries.getEntries()) durations.push(entry.duration); }).observe({ type: 'longtask', buffered: true });
    document.addEventListener('DOMContentLoaded', () => {
      const badge = document.createElement('div');
      badge.textContent = '100 SESSION / AGENT-LABEL FIXTURE · ACTUAL PRODUCT UI';
      badge.style.cssText = 'position:fixed;right:8px;bottom:8px;z-index:9999;background:#15191f;color:#efbf58;padding:6px;font:10px monospace';
      document.body.append(badge);
    });
  });
  await installApiFixtures(page);
  let parentageAvailable = true;
  await page.route('**/api/**/loom/temporal?*', (route) => route.fulfill({ json: { ...base, payload: loaded } }));
  await page.route('**/api/**/analytics/subagent-tree', (route) => route.fulfill({ json: { ...base, payload: parentageAvailable ? hierarchy : { ...hierarchy, available: false, error: 'fixture_parent_authority_unavailable', nodes: [] } } }));
  await page.route('**/api/**/hermes-lcm/session/**', (route) => {
    const sessionId = decodeURIComponent(new URL(route.request().url()).pathname.split('/session/')[1]!);
    return route.fulfill({ json: { ...base, payload: pageFor(sessionId) } });
  });
  const painted = () => page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
  const measure = async (name: string, action: () => Promise<unknown>) => {
    const begin = performance.now(); await action(); await painted(); timings[name] = Math.round((performance.now() - begin) * 10) / 10;
    peakDomNodes = Math.max(peakDomNodes, await page.locator("*").count());
  };
  await measure('initialNavigation', async () => {
    await page.goto(`${baseURL}/loom?scope=tracedecay&scopeLabel=TraceDecay`);
    await page.getByRole('button', { name: 'Expand branch Fixture agent 0', exact: true }).waitFor();
  });
  await page.evaluate(`(() => {
    const state = { samples: [], running: true };
    window.loomFrames = state;
    let previous = null;
    function frame(now) {
      if (previous !== null) state.samples.push(now - previous);
      previous = now;
      if (state.running) requestAnimationFrame(frame);
    }
    requestAnimationFrame(frame);
  })()`);
  // Dense default: one lane per root, three of them bundles; every session still has an exact table row.
  assert.equal(await page.locator('[data-lane-row]').count(), ROOTS);
  assert.equal(await page.locator('[data-cluster]').count(), BUNDLES);
  assert.equal(await page.locator('[data-navigator-lane]').count(), 100);
  assert.equal(await page.locator('[data-scene-layer="canvas"]').count(), 1);
  assert.equal(await page.locator('[data-event][data-kind="spawn"]').count(), 0);
  const viewportBefore = await page.locator('[data-scene-viewport]').getAttribute('x');
  await measure('expandBranch', async () => {
    await page.getByRole('button', { name: 'Expand branch Fixture agent 0', exact: true }).click();
    await page.getByRole('button', { name: 'Collapse branch Fixture agent 0', exact: true }).waitFor();
  });
  const expandedLanes = ROOTS + descendantsOf(0);
  assert.equal(await page.locator('[data-lane-row]').count(), expandedLanes);
  assert.equal(await page.locator('[data-event][data-kind="spawn"]').count(), descendantsOf(0));
  assert.equal(new URL(page.url()).searchParams.has('loomExpanded'), true);
  const stroke = await page.locator('[data-event][data-kind="spawn"] circle').nth(1).evaluate((node) => getComputedStyle(node).stroke);
  assert.notEqual(stroke, 'none');
  await page.screenshot({ path: path.join(out, '100-sessions-expanded.png') });
  await measure('minimapLocate', async () => {
    const minimap = page.getByRole('group', { name: 'Temporal minimap' });
    await page.getByRole('button', { name: 'Zoom in', exact: true }).click();
    await minimap.click({ position: { x: 300, y: 30 } });
    await page.waitForFunction((before) => document.querySelector('[data-scene-viewport]')?.getAttribute('x') !== before, viewportBefore);
  });
  assert.equal(new URL(page.url()).searchParams.has('loomWindow'), true);
  await page.screenshot({ path: path.join(out, '100-sessions-minimap.png') });
  await measure('collapseBranch', async () => {
    await page.getByRole('button', { name: 'Collapse branch Fixture agent 0', exact: true }).click();
    await page.getByRole('button', { name: 'Expand branch Fixture agent 0', exact: true }).waitFor();
  });
  assert.equal(await page.locator('[data-lane-row]').count(), ROOTS);
  await measure('fit', () => page.getByRole('button', { name: 'Return field to loaded tail' }).click());
  assert.equal(new URL(page.url()).searchParams.has('loomWindow'), false);
  await measure('openSession', async () => {
    await page.getByRole('button', { name: 'Open session Fixture agent 50', exact: true }).click();
    await page.locator('[data-event$=":event-199"]').waitFor();
  });
  assert.equal(await transcriptNodes(page).count(), 200);
  await measure('pickDenseEvent', () => page.locator('[data-event$=":event-100"]').click());
  assert.equal(new URL(page.url()).searchParams.get('loomEvent'), 'event-100');
  assert.equal(await transcriptNodes(page).count(), 101);
  assert.equal(await page.locator('[data-cursor]').count(), 1);
  await measure('stepReplay', () => page.getByRole('button', { name: 'Step to next stored event' }).click());
  assert.equal(new URL(page.url()).searchParams.get('loomEvent'), 'event-101');
  await measure('zoomReplay', () => page.getByRole('button', { name: 'Zoom in', exact: true }).click());
  const replayWindow = new URL(page.url()).searchParams.get('loomWindow');
  assert.ok(replayWindow);
  await page.screenshot({ path: path.join(out, '200-events-replay.png') });
  await measure('returnToOverview', () => page.getByRole('button', { name: '← All loaded sessions' }).click());
  assert.equal(new URL(page.url()).searchParams.get('loomWindow'), replayWindow);
  assert.equal(new URL(page.url()).searchParams.has('loomSession'), false);
  await page.getByRole('button', { name: 'Return field to loaded tail' }).click();
  const frames = await page.evaluate(() => {
    const state = (window as unknown as { loomFrames: { samples: number[]; running: boolean } }).loomFrames;
    state.running = false;
    return state.samples;
  });
  frames.sort((a, b) => a - b);
  previousLongTasks = await page.evaluate(() => (window as unknown as { loomLongTasks: number[] }).loomLongTasks);
  parentageAvailable = false;
  await measure('missingAuthority', async () => { await page.reload(); await page.getByText(/Session hierarchy: unavailable/).waitFor(); });
  // Without parentage every session is its own root and none has descendants,
  // so nothing bundles; the page-wide gap names the missing authority.
  assert.equal(await page.locator('[data-lane-row]').count(), 100);
  assert.equal(await page.locator('[data-event][data-kind="spawn"]').count(), 0);
  assert.equal(await page.locator('[data-cluster]').count(), 0);
  assert.match(await page.getByRole('list', { name: 'Evidence gaps' }).textContent() ?? '', /parentage unavailable/);
  await page.screenshot({ path: path.join(out, '100-sessions-parentage-unavailable.png') });
  const metrics = await page.evaluate(() => ({ domNodes: document.querySelectorAll('*').length, longTasks: (window as unknown as { loomLongTasks: number[] }).loomLongTasks }));
  const cdp = await page.context().newCDPSession(page);
  const heap = await cdp.send('Runtime.getHeapUsage');
  await cdp.detach();
  metrics.longTasks = [...previousLongTasks, ...metrics.longTasks];
  assert.deepEqual(errors, []);
  const report = { browser: browser.version(), viewport: '1440x1000', data: 'Explicit fixture: 100 provider-qualified sessions, 100 distinct agent labels; labels are not a production unique-agent census', loadedPageLimit: 200, sessions: 100, parentLinks: 96, missingParents: 2, timingsMs: timings, metrics: { ...metrics, peakDomNodes, interactionFrameIntervalsMs: { count: frames.length, median: frames[Math.floor(frames.length / 2)], p95: frames[Math.floor(frames.length * .95)], max: frames.at(-1) }, heapUsedBytes: heap.usedSize, heapTotalBytes: heap.totalSize }, errors };
  writeFileSync(path.join(out, 'measurements.json'), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report));
} finally {
  await browser.close();
  await new Promise<void>((resolve) => server.close(() => resolve()));
}
