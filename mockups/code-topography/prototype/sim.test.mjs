/**
 * Numeric contract for the TRACE simulation.
 *
 *   node --test mockups/code-topography/prototype/
 *
 * No framework, no install: `node:test` and `node:assert/strict` only, which is
 * the point — the simulation is pure enough to be tested without a browser, a
 * bundler or a dependency, and every clause of the sensory contract that can be
 * stated as a number is stated here rather than in prose.
 *
 * Tolerances are named and justified where they appear. Where a claim can be
 * exact — determinism, reduced-motion equivalence — it is asserted EXACTLY, by
 * bitwise array comparison, because a tolerance there would be hiding drift.
 */
import test from 'node:test';
import assert from 'node:assert/strict';

import { createSimulation, hopDistances, bloomStep, DEFAULT_PARAMS } from './sim.js';
import { DATASET, buildSimSpec, hopRings } from './dataset.js';

const DT = 1 / 60;

/**
 * A single anchored body with no channels at all — the weight channel in
 * isolation, so a mass claim cannot be contaminated by a neighbour's pull.
 */
function loneBody(mass, params) {
  return createSimulation({
    seed: 7,
    params: { ...params, jitter: 0 },
    nodes: [{ id: 'a', mass, x0: 0, y0: 0 }],
    springs: [],
  });
}

/** Two bodies joined by one channel of a chosen stiffness. */
function pair(stiffness, { mass = 20, separation = 200, params } = {}) {
  return createSimulation({
    seed: 11,
    params: { ...params, jitter: 0 },
    nodes: [
      { id: 'a', mass, x0: 0, y0: 0 },
      { id: 'b', mass, x0: separation, y0: 0 },
    ],
    springs: [{ a: 'a', b: 'b', stiffness }],
  });
}

/**
 * Settling time of a released body, in frames, by the classic envelope
 * definition: the last frame at which the body is still further than `tol` of
 * its pull from home.
 *
 * A speed threshold cannot be used here. An underdamped body passes through
 * zero speed at every turning point, so "first frame under 0.6 px/s" samples
 * WHERE IN THE SWING the frame happened to land and is not monotone in mass —
 * measured 115 frames at mass 20 against 71 at mass 40, which is a
 * frame-quantisation artefact, not a lighter-feeling hub. The envelope is what
 * a reader actually perceives as "it has stopped".
 */
function framesToRest(sim, id, offsetX, tol = 0.02) {
  const anchor = sim.anchorOf(id);
  sim.applyDrag(id, anchor.x + offsetX, anchor.y);
  sim.step(DT);
  sim.release();
  const threshold = Math.abs(offsetX) * tol;
  let last = 0;
  for (let frame = 1; frame <= 4000; frame += 1) {
    sim.step(DT);
    if (Math.abs(sim.positionOf(id).x - anchor.x) >= threshold) last = frame;
  }
  return last;
}

test('construction rejects malformed measurements rather than guessing', () => {
  assert.throws(() => createSimulation({ nodes: [], springs: [] }), /at least one node/);
  assert.throws(
    () => createSimulation({ nodes: [{ id: 'a', mass: 1, x0: 0, y0: 0 }, { id: 'a', mass: 1, x0: 1, y0: 1 }], springs: [] }),
    /duplicate node id a/,
  );
  assert.throws(
    () => createSimulation({ nodes: [{ id: 'a', mass: 1, x0: 0, y0: 0 }], springs: [{ a: 'a', b: 'ghost', stiffness: 1 }] }),
    /unknown node ghost/,
  );
  assert.throws(
    () => createSimulation({ nodes: [{ id: 'a', mass: 1, x0: Number.NaN, y0: 0 }], springs: [] }),
    /nodes\[0\]\.x0 must be a finite number/,
  );
  const sim = createSimulation({ nodes: [{ id: 'a', mass: 1, x0: 0, y0: 0 }], springs: [] });
  assert.throws(() => sim.step(0), /dt must be > 0/);
  assert.throws(() => sim.positionOf('ghost'), /unknown node id ghost/);
});

test('the layout is an exact equilibrium: rest length is anchor separation', () => {
  const sim = pair(30);
  // With jitter off and every spring at rest length, nothing should move at all.
  for (let i = 0; i < 240; i += 1) sim.step(DT);
  assert.equal(sim.maxSpeed(), 0);
  assert.deepEqual(Array.from(sim.positions()), [0, 0, 200, 0]);
  const energy = sim.energy();
  assert.equal(energy.total, 0);
});

test('WEIGHT: settle time increases monotonically with mass', () => {
  const masses = [4, 9, 20, 40, 63];
  const settle = masses.map((mass) => framesToRest(loneBody(mass), 'a', 160));
  for (let i = 1; i < settle.length; i += 1) {
    assert.ok(
      settle[i] > settle[i - 1],
      `mass ${masses[i]} settled in ${settle[i]} frames, not slower than mass ${masses[i - 1]} at ${settle[i - 1]}`,
    );
  }
  // The spread is the feel budget: enough to be felt, not enough to stall. The
  // exponent that produces it is a documented tuning knob, so this asserts the
  // BAND rather than an exact figure.
  const spread = settle[settle.length - 1] / settle[0];
  assert.ok(spread > 1.5 && spread < 4, `mass spread ${spread.toFixed(2)}x outside the 1.5–4x feel budget`);
});

test('WEIGHT: hover bloom latency increases with mass, and both extremes still arrive', () => {
  const halfBloomSeconds = (mass) => {
    let value = 0;
    let seconds = 0;
    while (value < 0.5 && seconds < 10) {
      value = bloomStep(value, 1, mass, DT);
      seconds += DT;
    }
    return seconds;
  };
  const leaf = halfBloomSeconds(4);
  const hub = halfBloomSeconds(63);
  assert.ok(hub > leaf * 2, `hub half-bloom ${hub.toFixed(3)}s is not meaningfully later than leaf ${leaf.toFixed(3)}s`);
  assert.ok(leaf < 0.2, `a leaf must flick: ${leaf.toFixed(3)}s to half bloom`);
  assert.ok(hub < 0.6, `a hub must still arrive: ${hub.toFixed(3)}s to half bloom`);
  // Release is slower than attack, so a bloom lingers rather than snapping off.
  assert.ok(bloomStep(1, 0, 20, DT) > 1 - (1 - bloomStep(0, 1, 20, DT)));
});

test('TENSION: a stiff channel propagates displacement, a weak one does not', () => {
  const STIFF_CALLS = 58;
  const WEAK_CALLS = 3;
  const PULL = 200;

  function follow(calls) {
    const sim = pair(calls);
    const start = sim.positionOf('b').x;
    // Hold 'a' displaced long enough for the neighbourhood to reach the
    // deformed equilibrium — this measures coupling, not the transient.
    for (let i = 0; i < 600; i += 1) {
      sim.applyDrag('a', -PULL, 0);
      sim.step(DT);
    }
    return Math.abs(sim.positionOf('b').x - start) / PULL;
  }

  const stiff = follow(STIFF_CALLS);
  const weak = follow(WEAK_CALLS);
  assert.ok(stiff >= 0.25, `stiff channel follow ratio ${stiff.toFixed(4)} below the 0.25 floor`);
  assert.ok(weak < 0.06, `weak channel follow ratio ${weak.toFixed(4)} above the 0.06 ceiling`);
  assert.ok(stiff / weak > 8, `stiff/weak contrast ${(stiff / weak).toFixed(1)}x is not legible as a difference in kind`);
});

test('TENSION: on the real subgraph, deformation falls off with hop distance', () => {
  const sim = createSimulation(buildSimSpec(DATASET, { seed: 3 }));
  sim.settle({ dt: DT });
  const before = sim.positions();
  const ids = sim.nodeIds;
  const hops = hopDistances(sim, DATASET.focusId);
  const anchor = sim.anchorOf(DATASET.focusId);
  for (let i = 0; i < 600; i += 1) {
    sim.applyDrag(DATASET.focusId, anchor.x - 180, anchor.y + 90);
    sim.step(DT);
  }
  const after = sim.positions();
  const moved = new Map(
    ids.map((id, i) => [
      id,
      Math.hypot(after[i * 2] - before[i * 2], after[i * 2 + 1] - before[i * 2 + 1]),
    ]),
  );

  const byHop = new Map();
  for (const [id, hop] of hops) {
    if (hop === 0) continue;
    byHop.set(hop, Math.max(byHop.get(hop) ?? 0, moved.get(id)));
  }
  const oneHop = byHop.get(1);
  const threeHop = byHop.get(3) ?? 0;
  assert.ok(oneHop > 12, `one-hop neighbours barely moved (${oneHop.toFixed(2)} px) — coupling is not being felt`);
  assert.ok(
    threeHop < oneHop * 0.35,
    `three-hop displacement ${threeHop.toFixed(2)} px is not clearly less than one-hop ${oneHop.toFixed(2)} px`,
  );
});

test('RELEASE: total energy decays monotonically and nearly to nothing', () => {
  const sim = createSimulation(buildSimSpec(DATASET, { seed: 5 }));
  sim.settle({ dt: DT });
  const anchor = sim.anchorOf(DATASET.focusId);
  for (let i = 0; i < 120; i += 1) {
    sim.applyDrag(DATASET.focusId, anchor.x - 220, anchor.y);
    sim.step(DT);
  }
  sim.release();

  const first = sim.energy().total;
  assert.ok(first > 1000, `nothing was stored to decay (${first.toFixed(1)})`);
  let previous = first;
  let peak = first;
  for (let i = 0; i < 900; i += 1) {
    sim.step(DT);
    const total = sim.energy().total;
    // Tolerance is RELATIVE and tiny: it covers the discretisation error of a
    // symplectic step, not a physical energy gain. A real blow-up (undamped
    // ringing, stiff-spring explosion) fails this by orders of magnitude.
    assert.ok(
      total <= previous * (1 + 1e-6) + 1e-9,
      `energy rose at frame ${i}: ${previous.toFixed(6)} → ${total.toFixed(6)}`,
    );
    previous = total;
    peak = Math.max(peak, total);
  }
  assert.equal(peak, first, 'the post-release peak must be the release itself, never later');
  assert.ok(previous < first * 1e-4, `energy only fell to ${(previous / first).toExponential(2)} of release`);
  assert.ok(sim.isSettled(), 'the field never came to rest after release');
});

test('RELEASE: the swing decays fast — one small overshoot, then nothing', () => {
  const PULL = 200;
  const sim = loneBody(63);
  const anchor = sim.anchorOf('a');
  sim.applyDrag('a', anchor.x + PULL, anchor.y);
  sim.step(DT);
  sim.release();

  // A damped sinusoid crosses zero forever, so counting crossings measures
  // float noise, not feel. What a reader actually perceives is the sequence of
  // swing amplitudes, so that is what is asserted.
  const swings = [];
  let previous = sim.positionOf('a').x - anchor.x;
  let extreme = previous;
  for (let i = 0; i < 3000; i += 1) {
    sim.step(DT);
    const offset = sim.positionOf('a').x - anchor.x;
    if (Math.sign(offset) !== Math.sign(previous) && Math.sign(offset) !== 0) {
      swings.push(Math.abs(extreme));
      extreme = offset;
    } else if (Math.abs(offset) > Math.abs(extreme)) {
      extreme = offset;
    }
    previous = offset;
    if (swings.length >= 3) break;
  }
  assert.ok(swings.length >= 2, 'the body never swung back — check the release path');
  assert.ok(swings[0] >= PULL * 0.9, `the first swing should be the pull itself, measured ${swings[0].toFixed(1)} px`);
  assert.ok(swings[1] < PULL * 0.12, `overshoot ${swings[1].toFixed(2)} px is more than 12 % of the pull`);
  if (swings[2] !== undefined) {
    assert.ok(swings[2] < PULL * 0.02, `second swing ${swings[2].toFixed(3)} px is still visible — this is ringing`);
  }
});

test('DETERMINISM: same seed and same gesture script give an identical trajectory', () => {
  const script = [
    { frames: 30, drag: null },
    { frames: 45, drag: { id: 'focus', dx: -170, dy: 60 } },
    { frames: 12, drag: { id: 'hctx', dx: 90, dy: -40 } },
    { frames: 120, drag: null },
  ];
  function run() {
    const sim = createSimulation(buildSimSpec(DATASET, { seed: 20260725 }));
    const trajectory = [];
    for (const phase of script) {
      if (!phase.drag) sim.release();
      for (let i = 0; i < phase.frames; i += 1) {
        if (phase.drag) {
          const anchor = sim.anchorOf(phase.drag.id);
          sim.applyDrag(phase.drag.id, anchor.x + phase.drag.dx, anchor.y + phase.drag.dy);
        }
        sim.step(DT);
        trajectory.push(Array.from(sim.positions()));
      }
    }
    return trajectory;
  }
  const a = run();
  const b = run();
  assert.equal(a.length, 207);
  assert.deepEqual(a, b, 'two runs of the same script diverged');
  // A different seed must actually change the trajectory, or the seed is a lie.
  const other = createSimulation(buildSimSpec(DATASET, { seed: 999 }));
  other.step(DT);
  const same = createSimulation(buildSimSpec(DATASET, { seed: 20260725 }));
  same.step(DT);
  assert.notDeepEqual(Array.from(other.positions()), Array.from(same.positions()));
});

test('REDUCED MOTION: settling in one shot lands on bit-identical positions', () => {
  // Reduced motion is not an approximation of the animated path — it is the
  // same `step()` sequence with the paints removed. So the arrays must match
  // EXACTLY, and this test would catch any renderer-side shortcut that broke
  // that equivalence.
  const animated = createSimulation(buildSimSpec(DATASET, { seed: 42 }));
  const frames = animated.settle({ dt: DT });
  assert.ok(frames > 5 && frames < 1200, `startup settled in ${frames} frames`);

  const reduced = createSimulation(buildSimSpec(DATASET, { seed: 42 }));
  for (let i = 0; i < frames; i += 1) reduced.step(DT);

  assert.deepEqual(Array.from(reduced.positions()), Array.from(animated.positions()));

  // And with a gesture in the middle: hold, settle, release, settle.
  const anchor = animated.anchorOf('hctx');
  for (const sim of [animated, reduced]) {
    for (let i = 0; i < 90; i += 1) {
      sim.applyDrag('hctx', anchor.x - 140, anchor.y + 70);
      sim.step(DT);
    }
    sim.release();
  }
  const settledFrames = animated.settle({ dt: DT });
  for (let i = 0; i < settledFrames; i += 1) reduced.step(DT);
  assert.deepEqual(Array.from(reduced.positions()), Array.from(animated.positions()));
  // Final positions must be the layout again: release returns the field home.
  animated.nodeIds.forEach((id) => {
    const home = animated.anchorOf(id);
    const now = animated.positionOf(id);
    assert.ok(
      Math.hypot(now.x - home.x, now.y - home.y) < 1,
      `${id} settled ${Math.hypot(now.x - home.x, now.y - home.y).toFixed(2)} px away from its layout anchor`,
    );
  });
});

test('readback surfaces are copies, not live views into the integrator', () => {
  const sim = createSimulation(buildSimSpec(DATASET, { seed: 1 }));
  const snapshot = sim.positions();
  snapshot[0] = 1e9;
  sim.step(DT);
  assert.notEqual(sim.positions()[0], 1e9);
  assert.notEqual(sim.positionOf('focus').x, 1e9);
  const params = sim.params;
  assert.throws(() => {
    'use strict';
    params.anchorBase = 1;
  });
});

test('substepping is a refinement, not a different simulation', () => {
  // One 1/60 step must equal four 1/240 steps taken through the public API,
  // which is what lets the page choose its frame budget without changing feel.
  const coarse = createSimulation({ ...buildSimSpec(DATASET, { seed: 8 }), params: { substep: 1 / 240 } });
  const fine = createSimulation({ ...buildSimSpec(DATASET, { seed: 8 }), params: { substep: 1 / 240 } });
  coarse.step(1 / 60);
  for (let i = 0; i < 4; i += 1) fine.step(1 / 240);
  assert.deepEqual(Array.from(coarse.positions()), Array.from(fine.positions()));
  assert.equal(coarse.substepCount, fine.substepCount);
});

test('every drawn row IS the measured hop ring', () => {
  // The row caption is "hop distance from the focus, not elevation", so a node's
  // ring and its measured distance cannot be allowed to disagree — that caption
  // is the whole reason the sheet is not a decorative flow diagram.
  const rings = hopRings(DATASET, DATASET.focusId);
  const expected = { u3: 3, u2: 2, u1: 1, focus: 0, d1: 1, d2: 2, d3: 3 };
  assert.equal(rings.size, DATASET.nodes.length, 'the drawn subgraph must be connected through the focus');
  for (const node of DATASET.nodes) {
    assert.equal(rings.get(node.id), expected[node.row], `${node.id} is drawn on ring ${node.row}`);
  }
  // The rule that makes that true is that an in-membrane move costs zero hops.
  // Assert the rule has teeth: dropping it moves nodes off their rows.
  const naive = hopDistances(createSimulation(buildSimSpec(DATASET, { seed: 1 })), DATASET.focusId);
  const disagreeing = DATASET.nodes.filter((node) => naive.get(node.id) !== expected[node.row]);
  assert.deepEqual(
    disagreeing.map((node) => node.id).sort(),
    ['acquire', 'adjacency', 'checkout', 'edgeindex', 'fetche', 'fetchn', 'neighbors', 'sibcall', 'sibgraph'],
    'the set of nodes reached through an in-membrane channel changed',
  );
});

test('default parameters are the ones the README documents', () => {
  assert.deepEqual(
    { ...DEFAULT_PARAMS },
    {
      anchorBase: 90,
      anchorMassExponent: 0.5,
      edgeStiffnessScale: 6,
      dampingRatio: 0.72,
      substep: 1 / 240,
      restSpeed: 0.6,
      minMass: 3,
      jitter: 6,
      bloomAttack: 9.5,
      bloomRelease: 5,
      bloomMassExponent: 0.42,
    },
  );
});
