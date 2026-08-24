/**
 * QA driver for the TRACE live prototype.
 *
 * Round two is a claim about feel, so it needs a gate that can fail. This script
 * opens the page in Chromium, performs REAL pointer gestures (synthesised mouse
 * events, not a scripted call into the simulation — the harness can only be
 * asked where a node is, never told to move one), records every frame with the
 * page's own flight recorder, and asserts on the recording:
 *
 *   · the dragged node led the field
 *   · its stiffest-coupled neighbour followed by at least a stated fraction
 *   · nodes three or more channels away stayed put
 *   · the field settled inside a stated number of frames after release
 *   · frame-time p95 stayed under one 60 Hz budget
 *
 * The same gestures also produce the keyframe screenshots. Playwright is not a
 * dependency of this folder — it is resolved out of the dashboard's
 * node_modules, the only place in the repo that has it, and the location comes
 * from an environment variable so no machine-local path is ever committed:
 *
 *   TD_DASHBOARD_DIR   directory holding the dashboard's node_modules.
 *                      Default: ../../../dashboard, relative to this script.
 *
 *   node qa-drive.mjs
 *   TD_DASHBOARD_DIR=/elsewhere/dashboard node qa-drive.mjs
 *   node qa-drive.mjs --serve      # just host the page for manual review
 *
 * ES modules are blocked over file://, so the page is always served over a
 * loopback HTTP server on an ephemeral port, started and stopped by this script.
 */
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import http from 'node:http';
import path from 'node:path';
import fs from 'node:fs';

const here = path.dirname(fileURLToPath(import.meta.url));
const shotsDir = path.join(here, 'shots');

/* ---- tunable gate thresholds -------------------------------------------- */
/* Every one of these is a measured baseline, not an aspiration. The README
 * carries the numbers this run produced; the owner's feel feedback moves the
 * simulation parameters and these move with them. */
const GATE = Object.freeze({
  /** Fraction of the dragged node's travel the stiffest neighbour must pick up. */
  stiffFollowFloor: 0.1,
  /** Ceiling for a LOOSE neighbour, so "coupled" and "trailing" differ in kind. */
  looseFollowCeiling: 0.1,
  /**
   * Max travel for a node three or more channels away, as a fraction of the
   * dragged node's travel. Expressed as a ratio rather than in px so the gate
   * does not silently loosen when the gesture length is tuned.
   */
  farDisplacementCeiling: 0.05,
  /** Frames from release to a field where nothing moves more than 0.01 px. */
  settleFrameCeiling: 200,
  /** One 60 Hz frame: the budget the page's own step+paint work must fit in. */
  workMsP95Ceiling: 16.7,
  /**
   * Ceiling on the frame INTERVAL. A page holding 60 Hz has an interval of
   * 16.7 ms by definition, so this can only ever detect dropped frames — 25 ms
   * is one and a half frames.
   */
  frameIntervalP95Ceiling: 25,
  /** Length of the drag gesture, in world px, along the tested channel's axis. */
  dragDistance: 200,
  /** Keep the gesture target this far inside the field. */
  fieldMargin: 70,
  /** Speed, in px/s, below which the field counts as genuinely at rest. */
  restSpeed: 0.02,
});

/* ---- playwright ---------------------------------------------------------- */
const dashboardDir = path.resolve(here, process.env.TD_DASHBOARD_DIR ?? '../../../dashboard');
function loadChromium() {
  const requireFromDashboard = createRequire(path.join(dashboardDir, 'package.json'));
  try {
    return requireFromDashboard('playwright').chromium;
  } catch (cause) {
    throw new Error(
      `Could not load playwright from ${dashboardDir}. ` +
        'Set TD_DASHBOARD_DIR to a directory whose node_modules contains playwright.',
      { cause },
    );
  }
}

/* ---- static server: ES modules need an origin --------------------------- */
const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.json': 'application/json', '.css': 'text/css' };

function startServer() {
  const server = http.createServer((request, response) => {
    const requested = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    const target = path.join(here, requested === '/' ? 'trace-live.html' : requested);
    // Refuse anything outside the prototype folder, so a stray relative path in
    // the page cannot turn this into a repo file server.
    if (!target.startsWith(here + path.sep) && target !== here) {
      response.writeHead(403).end('forbidden');
      return;
    }
    fs.readFile(target, (error, body) => {
      if (error) {
        response.writeHead(404).end('not found');
        return;
      }
      response.writeHead(200, { 'content-type': MIME[path.extname(target)] ?? 'application/octet-stream' });
      response.end(body);
    });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({ server, port: server.address().port }));
  });
}

/* ---- result collection: report every failure, not just the first -------- */
const results = [];
function check(label, passed, detail) {
  results.push({ label, passed, detail });
  console.log(`  ${passed ? 'PASS' : 'FAIL'}  ${label} — ${detail}`);
}
const measurements = {};

/* ---- take analysis (mirrors recorder.js helpers, run in Node) ---------- */
function displacementsBetween(take, fromFrame, toFrame) {
  const a = take.frames[fromFrame];
  const b = take.frames[toFrame];
  const out = new Map();
  take.nodeIds.forEach((id, i) => {
    out.set(
      id,
      Math.hypot(b.positions[i * 2] - a.positions[i * 2], b.positions[i * 2 + 1] - a.positions[i * 2 + 1]),
    );
  });
  return out;
}

function settleFrame(take, fromFrame, restPx = 0.01) {
  for (let i = Math.max(1, fromFrame + 1); i < take.frames.length; i += 1) {
    const previous = take.frames[i - 1].positions;
    const current = take.frames[i].positions;
    let worst = 0;
    for (let p = 0; p < current.length; p += 2) {
      const step = Math.hypot(current[p] - previous[p], current[p + 1] - previous[p + 1]);
      if (step > worst) worst = step;
    }
    if (worst < restPx) return i;
  }
  return -1;
}

function markFrame(take, label) {
  const mark = take.marks.find((entry) => entry.label === label);
  return mark ? Math.min(mark.frame, take.frames.length - 1) : -1;
}

/* ---- the run ------------------------------------------------------------ */
async function waitForRest(page, restSpeed = GATE.restSpeed) {
  await page.waitForFunction(
    (speed) => window.__traceHarness?.ready && window.__traceHarness.isSettled(speed),
    restSpeed,
    { timeout: 20000 },
  );
}

async function shoot(page, name) {
  const file = path.join(shotsDir, `${name}.png`);
  await page.locator('.hero').screenshot({ path: file });
  console.log(`  shot  shots/${name}.png`);
  return file;
}

/**
 * One gesture: grab a node, drag it across the field, hold until the
 * neighbourhood reaches its deformed equilibrium, release, watch it settle.
 * Keyframes at rest / mid-drag / release+100 ms / settled.
 */
async function runGesture(page, { name, nodeId, expectFollow }) {
  console.log(`\n[${name}] node ${nodeId}`);
  await page.evaluate(() => window.__traceHarness.reset());
  await waitForRest(page);

  await page.evaluate(() => window.__traceRecorder.start('rest'));
  // A few frames of genuine rest, so "before" is a real reading.
  await page.waitForTimeout(120);
  await shoot(page, `${name}-1-rest`);

  /**
   * The gesture runs ALONG the axis of the channel under test.
   *
   * This is not a convenience: dragging a hub across a channel barely changes
   * that channel's length, so the first run of this gate measured `focus`
   * pulling its stiffest neighbour `hctx` (k=204) only 2.5 % while the much
   * weaker `hsearch` (k=72) came along 11 % — purely because `hsearch` happened
   * to lie along the drag direction. A follow ratio read off an arbitrary
   * direction is a statement about layout geometry, not about coupling. Aiming
   * down the channel makes the reading a measurement of stiffness, and applying
   * the same rule to both gestures makes hub and leaf comparable.
   */
  const plan = await page.evaluate(
    ([id, distance, margin]) => {
      const h = window.__traceHarness;
      const world = h.world();
      const node = h.worldOf(id);
      const stiffest = h.stiffestNeighbour(id);
      const other = h.worldOf(stiffest.other);
      const length = Math.hypot(node.x - other.x, node.y - other.y) || 1;
      const axis = { x: (node.x - other.x) / length, y: (node.y - other.y) / length };
      const inside = (point) =>
        point.x > margin &&
        point.y > margin &&
        point.x < world.width - margin &&
        point.y < world.height - margin;
      let reach = distance;
      let target = null;
      let direction = 'away';
      while (reach > 40 && target === null) {
        const away = { x: node.x + axis.x * reach, y: node.y + axis.y * reach };
        const toward = { x: node.x - axis.x * reach, y: node.y - axis.y * reach };
        if (inside(away)) {
          target = away;
          direction = 'away';
        } else if (inside(toward)) {
          target = toward;
          direction = 'toward';
        } else {
          reach -= 20;
        }
      }
      return {
        mass: h.massOf(id),
        channels: h.springsOf(id).length,
        hops: h.hopsFrom(id),
        stiffest,
        channelLength: length,
        reach,
        direction,
        from: h.screenOf(id),
        to: h.clientFromWorld(target.x, target.y),
      };
    },
    [nodeId, GATE.dragDistance, GATE.fieldMargin],
  );
  const facts = plan;
  console.log(
    `  gesture: ${plan.reach.toFixed(0)} px ${plan.direction} from ${plan.stiffest.other} ` +
      `along a ${plan.channelLength.toFixed(0)} px channel of stiffness ${plan.stiffest.stiffness}`,
  );

  const path0 = { from: plan.from, to: plan.to };
  await page.mouse.move(path0.from.x, path0.from.y);
  await page.waitForTimeout(80);
  await page.mouse.down();
  const STEPS = 20;
  for (let i = 1; i <= STEPS; i += 1) {
    const t = i / STEPS;
    await page.mouse.move(
      path0.from.x + (path0.to.x - path0.from.x) * t,
      path0.from.y + (path0.to.y - path0.from.y) * t,
    );
    await page.waitForTimeout(16);
    if (i === Math.round(STEPS / 2)) await shoot(page, `${name}-2-mid-drag`);
  }
  // Hold: the follow ratio is a coupling measurement, so it is read at the
  // deformed equilibrium rather than off the transient.
  await page.waitForTimeout(700);
  await page.evaluate(() => window.__traceRecorder.mark('held'));
  await page.waitForTimeout(40);

  await page.mouse.up();
  await page.waitForTimeout(100);
  await shoot(page, `${name}-3-release-plus-100ms`);

  await waitForRest(page);
  await page.waitForTimeout(60);
  await shoot(page, `${name}-4-settled`);
  const take = await page.evaluate(() => window.__traceRecorder.stop());

  /* ---- assertions on the recording ------------------------------------- */
  const restFrame = markFrame(take, 'rest');
  const heldFrame = markFrame(take, 'held');
  const releaseFrame = markFrame(take, 'release');
  check(
    `[${name}] recording has rest / held / release marks`,
    restFrame >= 0 && heldFrame > restFrame && releaseFrame > heldFrame,
    `frames ${restFrame} / ${heldFrame} / ${releaseFrame} of ${take.frameCount}`,
  );

  const moved = displacementsBetween(take, restFrame, heldFrame);
  const dragged = moved.get(nodeId);
  const others = [...moved].filter(([id]) => id !== nodeId);
  const runnerUp = others.reduce((best, entry) => (entry[1] > best[1] ? entry : best), ['—', 0]);
  check(
    `[${name}] the dragged node led the field`,
    dragged > runnerUp[1],
    `${nodeId} moved ${dragged.toFixed(1)} px; next was ${runnerUp[0]} at ${runnerUp[1].toFixed(1)} px`,
  );

  const neighbour = facts.stiffest.other;
  const followRatio = moved.get(neighbour) / dragged;
  if (expectFollow === 'coupled') {
    check(
      `[${name}] stiffest neighbour followed as flesh`,
      followRatio >= GATE.stiffFollowFloor,
      `${neighbour} (k=${facts.stiffest.stiffness}) followed ${(followRatio * 100).toFixed(1)} % ≥ ${GATE.stiffFollowFloor * 100} %`,
    );
  } else {
    check(
      `[${name}] loose neighbour trailed instead of following`,
      followRatio < GATE.looseFollowCeiling,
      `${neighbour} (k=${facts.stiffest.stiffness}) followed ${(followRatio * 100).toFixed(1)} % < ${GATE.looseFollowCeiling * 100} %`,
    );
  }

  const far = [...moved].filter(([id]) => (facts.hops[id] ?? 99) >= 3);
  const worstFar = far.reduce((best, entry) => (entry[1] > best[1] ? entry : best), ['—', 0]);
  const worstFarRatio = worstFar[1] / dragged;
  check(
    `[${name}] nodes ≥3 channels away stayed put`,
    far.length > 0 && worstFarRatio < GATE.farDisplacementCeiling,
    `${far.length} far nodes; worst ${worstFar[0]} at ${worstFar[1].toFixed(2)} px ` +
      `= ${(worstFarRatio * 100).toFixed(1)} % of the drag < ${GATE.farDisplacementCeiling * 100} %`,
  );

  const settledAt = settleFrame(take, releaseFrame);
  const settleFrames = settledAt < 0 ? -1 : settledAt - releaseFrame;
  check(
    `[${name}] settled after release`,
    settleFrames >= 0 && settleFrames <= GATE.settleFrameCeiling,
    `${settleFrames} frames ≤ ${GATE.settleFrameCeiling}`,
  );

  check(
    `[${name}] per-frame work p95 inside the 60 Hz budget`,
    take.workMs.p95 !== null && take.workMs.p95 < GATE.workMsP95Ceiling,
    `step+paint p50 ${take.workMs.p50?.toFixed(2)} ms, p95 ${take.workMs.p95?.toFixed(2)} ms, max ${take.workMs.max?.toFixed(2)} ms`,
  );

  check(
    `[${name}] frame interval p95 shows no dropped frames`,
    take.frameMs.p95 !== null && take.frameMs.p95 < GATE.frameIntervalP95Ceiling,
    `interval p50 ${take.frameMs.p50?.toFixed(2)} ms, p95 ${take.frameMs.p95?.toFixed(2)} ms ` +
      `(max ${take.frameMs.max?.toFixed(0)} ms — the screenshot pauses this driver itself causes)`,
  );

  check(
    `[${name}] no frames dropped from the take`,
    take.droppedFrames === 0,
    `${take.frameCount} frames captured, ${take.droppedFrames} dropped`,
  );

  measurements[name] = {
    nodeId,
    mass: facts.mass,
    channels: facts.channels,
    stiffestNeighbour: neighbour,
    stiffestStiffness: facts.stiffest.stiffness,
    draggedTravelPx: Number(dragged.toFixed(2)),
    followRatio: Number(followRatio.toFixed(4)),
    runnerUp: { id: runnerUp[0], px: Number(runnerUp[1].toFixed(2)) },
    worstFar: {
      id: worstFar[0],
      px: Number(worstFar[1].toFixed(3)),
      ratio: Number(worstFarRatio.toFixed(4)),
      count: far.length,
    },
    settleFrames,
    gesture: { reachPx: Math.round(plan.reach), direction: plan.direction, channelLengthPx: Math.round(plan.channelLength) },
    workMs: take.workMs,
    frameIntervalMs: take.frameMs,
    frameCount: take.frameCount,
  };
}

async function main() {
  const serveOnly = process.argv.includes('--serve');
  const { server, port } = await startServer();
  const url = `http://127.0.0.1:${port}/trace-live.html`;

  if (serveOnly) {
    console.log(`serving ${url}\npress ctrl-c to stop`);
    return;
  }

  fs.mkdirSync(shotsDir, { recursive: true });
  const chromium = loadChromium();
  const browser = await chromium.launch();
  // Tall enough that the whole 1440x1000 field is on screen without scrolling:
  // a scroll mid-gesture would move the client coordinates under the pointer.
  const context = await browser.newContext({
    viewport: { width: 1440, height: 1500 },
    deviceScaleFactor: 1,
    reducedMotion: 'no-preference',
    colorScheme: 'dark',
  });
  const page = await context.newPage();
  const problems = [];
  page.on('pageerror', (error) => problems.push(String(error)));
  page.on('console', (message) => {
    if (message.type() === 'error') problems.push(message.text());
  });

  try {
    await page.goto(url, { waitUntil: 'load' });
    await waitForRest(page);

    const identity = await page.evaluate(() => ({
      heaviest: window.__traceHarness.heaviestId(),
      leaf: window.__traceHarness.leafId(),
      params: window.__traceHarness.params(),
    }));
    console.log(`heaviest hub: ${identity.heaviest}   leaf: ${identity.leaf}`);
    measurements.params = identity.params;

    await runGesture(page, { name: 'hub', nodeId: identity.heaviest, expectFollow: 'coupled' });
    await runGesture(page, { name: 'leaf', nodeId: identity.leaf, expectFollow: 'loose' });

    // One light-theme settled shot: the renderer must answer a theme flip from
    // the token block, not from a second palette.
    console.log('\n[light] settled, light theme');
    await page.evaluate(() => {
      window.__traceHarness.reset();
      window.__traceHarness.setTheme('light');
    });
    await waitForRest(page);
    await page.waitForTimeout(120);
    await shoot(page, 'settled-light');
    await page.evaluate(() => window.__traceHarness.setTheme('dark'));

    // Reduced motion: the same field, drawn once, with tension carried by the
    // core rails instead of by movement.
    console.log('\n[reduced-motion] one-shot settle, no animation');
    const reduced = await page.evaluate(() => {
      const h = window.__traceHarness;
      h.reset();
      h.setReducedMotion(true);
      return { settled: h.isSettled(), frame: h.frameIndex(), reduced: h.reducedMotion() };
    });
    check(
      '[reduced-motion] one-shot settle reaches rest with no animation frames',
      reduced.reduced && reduced.settled,
      `settled=${reduced.settled} after ${reduced.frame} integrator frames, animation off`,
    );
    await shoot(page, 'reduced-motion-settled');
    const heldRails = await page.evaluate(async () => {
      const h = window.__traceHarness;
      const id = h.heaviestId();
      const world = h.worldOf(id);
      const from = h.screenOf(id);
      return { id, from, to: h.clientFromWorld(world.x - 180, world.y + 90) };
    });
    await page.mouse.move(heldRails.from.x, heldRails.from.y);
    await page.mouse.down();
    await page.mouse.move(heldRails.to.x, heldRails.to.y);
    await page.waitForTimeout(120);
    await shoot(page, 'reduced-motion-held');
    await page.mouse.up();
    await page.evaluate(() => window.__traceHarness.setReducedMotion(false));

    check('page raised no console or runtime errors', problems.length === 0, problems.join(' | ') || 'clean');
  } finally {
    await browser.close();
    server.close();
  }

  console.log(`\n${JSON.stringify(measurements, null, 2)}`);
  const failed = results.filter((result) => !result.passed);
  console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
  if (failed.length) {
    console.error(`FAILED:\n${failed.map((result) => `  ${result.label} — ${result.detail}`).join('\n')}`);
    process.exitCode = 1;
  }
}

await main();
