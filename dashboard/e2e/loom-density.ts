/** Exercise the shipped Loom with bounded, explicitly synthetic wire data.
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
    await page.getByRole('button', { name: 'Collapse branch Fixture agent 0', exact: true }).waitFor();
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
  assert.equal(await page.locator('[data-thread]').count(), 100);
  assert.equal(await page.locator('[data-parent-session]').count(), 96);
  const stroke = await page.locator('[data-parent-session] path').first().evaluate((node) => getComputedStyle(node).stroke);
  assert.notEqual(stroke, 'none');
  const viewportBefore = await page.locator('[data-session-viewport]').getAttribute('y');
  await measure('minimapLocate', async () => {
    const minimap = page.getByRole('group', { name: 'Session hierarchy minimap' });
    await minimap.click({ position: { x: 650, y: 65 } });
    await page.waitForFunction((before) => document.querySelector('[data-session-viewport]')?.getAttribute('y') !== before, viewportBefore);
  });
  await page.screenshot({ path: path.join(out, '100-sessions-minimap.png') });
  await measure('collapseBranch', () => page.getByRole('button', { name: 'Collapse branch Fixture agent 0', exact: true }).click());
  const collapsedCount = 100 - hierarchy.nodes.find((node) => node.session_id === 'fixture-session-0')!.descendants;
  assert.equal(await page.locator('[data-thread]').count(), collapsedCount);
  assert.equal(await page.locator('[data-minimap-session]').count(), collapsedCount);
  await measure('expandBranch', () => page.getByRole('button', { name: 'Expand branch Fixture agent 0', exact: true }).click());
  await measure('zoomOverview', () => page.getByRole('button', { name: 'Zoom in', exact: true }).click());
  const overviewWindow = new URL(page.url()).searchParams.get('loomOverviewWindow');
  await measure('openSession', async () => {
    await page.getByRole('button', { name: 'Open session Fixture agent 50', exact: true }).click();
    await page.getByRole('button', { name: 'Select stored event event-199', exact: true }).waitFor();
  });
  await measure('pickDenseEvent', () => page.getByRole('button', { name: 'Select stored event event-100', exact: true }).click());
  assert.equal(new URL(page.url()).searchParams.get('loomEvent'), 'event-100');
  assert.equal(await page.locator('[data-event]').count(), 101);
  assert.equal(await page.locator('[data-minimap-event]').count(), 101);
  await measure('stepReplay', () => page.getByRole('button', { name: 'Step to next stored event' }).click());
  assert.equal(new URL(page.url()).searchParams.get('loomEvent'), 'event-101');
  await measure('zoomReplay', () => page.getByRole('button', { name: 'Zoom into execution' }).click());
  await page.screenshot({ path: path.join(out, '200-events-replay.png') });
  await measure('returnToOverview', () => page.getByRole('button', { name: '← All loaded sessions' }).click());
  assert.equal(new URL(page.url()).searchParams.get('loomOverviewWindow'), overviewWindow);
  await page.getByRole('button', { name: 'Fit the whole extent' }).click();
  await page.screenshot({ path: path.join(out, '100-sessions-expanded.png') });
  const frames = await page.evaluate(() => {
    const state = (window as unknown as { loomFrames: { samples: number[]; running: boolean } }).loomFrames;
    state.running = false;
    return state.samples;
  });
  frames.sort((a, b) => a - b);
  previousLongTasks = await page.evaluate(() => (window as unknown as { loomLongTasks: number[] }).loomLongTasks);
  parentageAvailable = false;
  await measure('missingAuthority', async () => { await page.reload(); await page.getByText(/Session hierarchy: unavailable/).waitFor(); });
  assert.equal(await page.locator('[data-thread]').count(), 100);
  assert.equal(await page.locator('[data-parent-session]').count(), 0);
  assert.equal(await page.locator('[data-minimap-parent]').count(), 0);
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
