/**
 * Numeric contract for the TRACE simulation.
 *
 * Ported from the round-two prototype's `sim.test.mjs`
 * (`mockups/code-topography/prototype/`, branch
 * `worktree-agent-af882d6565fbab159`). The prototype ran these under
 * `node --test` against its hand-authored 26-node sheet; here they run under
 * vitest against a field built by `model.ts` from the wire-true neighbors
 * fixture, so the physics contract and the real payload shape are held to the
 * same assertions in one place.
 *
 * Tolerances are named and justified where they appear. Where a claim can be
 * exact — determinism, reduced-motion equivalence — it is asserted EXACTLY, by
 * array comparison, because a tolerance there would be hiding drift.
 */
import { describe, expect, it } from 'vitest';

import { resolveFixture } from '../../../stories/fixtures/data.ts';
import {
  DashboardEnvelopeV1Schema,
  GraphNeighborsPayloadV1Schema,
} from '../../contracts/generated.ts';
import {
  DEFAULT_PARAMS,
  bloomStep,
  createSimulation,
  hopDistances,
  type SimParams,
  type Simulation,
} from './sim.ts';
import { buildSimSpec, buildTraceModel, type NeighborsPayload } from './model.ts';
import type { TraceModel } from './types.ts';

const DT = 1 / 60;

function neighbors(id: string): NeighborsPayload {
  return DashboardEnvelopeV1Schema(GraphNeighborsPayloadV1Schema).parse(
    resolveFixture(`/api/plugins/graph/node/${id}/neighbors`),
  ).payload;
}

/** The field the drill-in actually draws for `sym-0`, hop 2, from fixtures. */
function fixtureModel(): TraceModel {
  const root = neighbors('sym-0');
  const hop1 = new Set<string>();
  for (const row of [...(root.callers ?? []), ...(root.callees ?? [])]) {
    if (typeof row.id === 'string') hop1.add(row.id);
  }
  const expanded = new Map<string, NeighborsPayload>();
  for (const id of [...hop1].slice(0, 12)) expanded.set(id, neighbors(id));
  return buildTraceModel({
    focus: { id: 'sym-0', kind: 'function', name: 'sym_0', degree: 24 },
    root,
    expanded,
  });
}

const MODEL = fixtureModel();

function fieldSim(seed: number): Simulation {
  return createSimulation(buildSimSpec(MODEL, seed));
}

/**
 * A single anchored body with no channels at all — the weight channel in
 * isolation, so a mass claim cannot be contaminated by a neighbour's pull.
 */
function loneBody(mass: number, params?: Partial<SimParams>): Simulation {
  return createSimulation({
    seed: 7,
    params: { ...params, jitter: 0 },
    nodes: [{ id: 'a', mass, x0: 0, y0: 0 }],
    springs: [],
  });
}

/** Two bodies joined by one channel of a chosen stiffness. */
function pair(
  stiffness: number,
  { mass = 20, separation = 200 }: { mass?: number; separation?: number } = {},
): Simulation {
  return createSimulation({
    seed: 11,
    params: { jitter: 0 },
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
 * WHERE IN THE SWING the frame happened to land and is not monotone in mass.
 * The envelope is what a reader actually perceives as "it has stopped".
 */
function framesToRest(sim: Simulation, id: string, offsetX: number, tol = 0.02): number {
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

describe('TRACE simulation', () => {
  it('rejects malformed measurements rather than guessing', () => {
    expect(() => createSimulation({ nodes: [], springs: [] })).toThrow(/at least one node/);
    expect(() =>
      createSimulation({
        nodes: [
          { id: 'a', mass: 1, x0: 0, y0: 0 },
          { id: 'a', mass: 1, x0: 1, y0: 1 },
        ],
        springs: [],
      }),
    ).toThrow(/duplicate node id a/);
    expect(() =>
      createSimulation({
        nodes: [{ id: 'a', mass: 1, x0: 0, y0: 0 }],
        springs: [{ a: 'a', b: 'ghost', stiffness: 1 }],
      }),
    ).toThrow(/unknown node ghost/);
    expect(() =>
      createSimulation({
        nodes: [{ id: 'a', mass: 1, x0: Number.NaN, y0: 0 }],
        springs: [],
      }),
    ).toThrow(/nodes\[0\]\.x0 must be a finite number/);
    const sim = createSimulation({ nodes: [{ id: 'a', mass: 1, x0: 0, y0: 0 }], springs: [] });
    expect(() => sim.step(0)).toThrow(/dt must be > 0/);
    expect(() => sim.positionOf('ghost')).toThrow(/unknown node id ghost/);
  });

  it('makes the layout an exact equilibrium: rest length is anchor separation', () => {
    const sim = pair(30);
    // With jitter off and every spring at rest length, nothing should move.
    for (let i = 0; i < 240; i += 1) sim.step(DT);
    expect(sim.maxSpeed()).toBe(0);
    expect(Array.from(sim.positions())).toEqual([0, 0, 200, 0]);
    expect(sim.energy().total).toBe(0);
  });

  it('WEIGHT: settle time increases monotonically with mass', () => {
    const masses = [4, 9, 20, 40, 63];
    const settle = masses.map((mass) => framesToRest(loneBody(mass), 'a', 160));
    for (let i = 1; i < settle.length; i += 1) {
      expect(
        settle[i]! > settle[i - 1]!,
        `mass ${masses[i]} settled in ${settle[i]} frames, not slower than mass ${masses[i - 1]} at ${settle[i - 1]}`,
      ).toBe(true);
    }
    // The spread is the feel budget: enough to be felt, not enough to stall.
    const spread = settle[settle.length - 1]! / settle[0]!;
    expect(spread).toBeGreaterThan(1.5);
    expect(spread).toBeLessThan(4);
  });

  it('WEIGHT: hover bloom latency increases with mass, and both extremes arrive', () => {
    const halfBloomSeconds = (mass: number): number => {
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
    expect(hub).toBeGreaterThan(leaf * 2);
    // A leaf must flick; a hub must still arrive.
    expect(leaf).toBeLessThan(0.2);
    expect(hub).toBeLessThan(0.6);
    // Release is slower than attack, so a bloom lingers rather than snapping off.
    expect(bloomStep(1, 0, 20, DT)).toBeGreaterThan(1 - (1 - bloomStep(0, 1, 20, DT)));
  });

  it('TENSION: a stiff channel propagates displacement, a weak one does not', () => {
    const PULL = 200;
    function follow(calls: number): number {
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
    const stiff = follow(58);
    const weak = follow(3);
    expect(stiff).toBeGreaterThanOrEqual(0.25);
    expect(weak).toBeLessThan(0.06);
    expect(stiff / weak).toBeGreaterThan(8);
  });

  it('TENSION: on the fixture subgraph, deformation falls off with hop distance', () => {
    const sim = fieldSim(3);
    sim.settle({ dt: DT });
    const before = sim.positions();
    const ids = sim.nodeIds;
    const hops = hopDistances(sim, MODEL.focusId);
    const anchor = sim.anchorOf(MODEL.focusId);
    for (let i = 0; i < 600; i += 1) {
      sim.applyDrag(MODEL.focusId, anchor.x - 180, anchor.y + 90);
      sim.step(DT);
    }
    const after = sim.positions();
    const moved = new Map(
      ids.map((id, i) => [
        id,
        Math.hypot(after[i * 2]! - before[i * 2]!, after[i * 2 + 1]! - before[i * 2 + 1]!),
      ]),
    );
    const byHop = new Map<number, number>();
    for (const [id, hop] of hops) {
      if (hop === 0) continue;
      byHop.set(hop, Math.max(byHop.get(hop) ?? 0, moved.get(id) ?? 0));
    }
    const oneHop = byHop.get(1) ?? 0;
    const threeHop = byHop.get(3) ?? 0;
    expect(oneHop, 'one-hop neighbours barely moved — coupling is not being felt').toBeGreaterThan(
      12,
    );
    expect(threeHop).toBeLessThan(oneHop * 0.35);
  });

  it('RELEASE: total energy decays monotonically and nearly to nothing', () => {
    const sim = fieldSim(5);
    sim.settle({ dt: DT });
    const anchor = sim.anchorOf(MODEL.focusId);
    for (let i = 0; i < 120; i += 1) {
      sim.applyDrag(MODEL.focusId, anchor.x - 220, anchor.y);
      sim.step(DT);
    }
    sim.release();

    const first = sim.energy().total;
    expect(first, 'nothing was stored to decay').toBeGreaterThan(1000);
    let previous = first;
    let peak = first;
    for (let i = 0; i < 900; i += 1) {
      sim.step(DT);
      const total = sim.energy().total;
      // Tolerance is RELATIVE and tiny: it covers the discretisation error of
      // a symplectic step, not a physical energy gain. A real blow-up fails
      // this by orders of magnitude.
      expect(total, `energy rose at frame ${i}`).toBeLessThanOrEqual(previous * (1 + 1e-6) + 1e-9);
      previous = total;
      peak = Math.max(peak, total);
    }
    expect(peak, 'the post-release peak must be the release itself').toBe(first);
    expect(previous).toBeLessThan(first * 1e-4);
    expect(sim.isSettled(), 'the field never came to rest after release').toBe(true);
  });

  it('RELEASE: the swing decays fast — one small overshoot, then nothing', () => {
    const PULL = 200;
    const sim = loneBody(63);
    const anchor = sim.anchorOf('a');
    sim.applyDrag('a', anchor.x + PULL, anchor.y);
    sim.step(DT);
    sim.release();

    // A damped sinusoid crosses zero forever, so counting crossings measures
    // float noise, not feel. What a reader perceives is the sequence of swing
    // amplitudes, so that is what is asserted.
    const swings: number[] = [];
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
    expect(swings.length, 'the body never swung back').toBeGreaterThanOrEqual(2);
    expect(swings[0]).toBeGreaterThanOrEqual(PULL * 0.9);
    expect(swings[1]!, 'overshoot is more than 12 % of the pull').toBeLessThan(PULL * 0.12);
    if (swings[2] !== undefined) {
      expect(swings[2], 'second swing is still visible — this is ringing').toBeLessThan(
        PULL * 0.02,
      );
    }
  });

  it('DETERMINISM: same seed and same gesture script give an identical trajectory', () => {
    const focus = MODEL.focusId;
    const other = MODEL.nodes.find((node) => node.id !== focus)!.id;
    const script = [
      { frames: 30, drag: null },
      { frames: 45, drag: { id: focus, dx: -170, dy: 60 } },
      { frames: 12, drag: { id: other, dx: 90, dy: -40 } },
      { frames: 120, drag: null },
    ] as const;
    function run(): number[][] {
      const sim = fieldSim(20260725);
      const trajectory: number[][] = [];
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
    expect(a.length).toBe(207);
    expect(a, 'two runs of the same script diverged').toEqual(b);
    // A different seed must actually change the trajectory, or the seed is a lie.
    const seeded = fieldSim(999);
    seeded.step(DT);
    const same = fieldSim(20260725);
    same.step(DT);
    expect(Array.from(seeded.positions())).not.toEqual(Array.from(same.positions()));
  });

  it('REDUCED MOTION: settling in one shot lands on identical positions', () => {
    // Reduced motion is not an approximation of the animated path — it is the
    // same `step()` sequence with the paints removed. So the arrays must match
    // EXACTLY, and this catches any renderer-side shortcut that broke it.
    const animated = fieldSim(42);
    const frames = animated.settle({ dt: DT });
    expect(frames).toBeGreaterThan(5);
    expect(frames).toBeLessThan(1200);

    const reduced = fieldSim(42);
    for (let i = 0; i < frames; i += 1) reduced.step(DT);
    expect(Array.from(reduced.positions())).toEqual(Array.from(animated.positions()));

    // And with a gesture in the middle: hold, settle, release, settle.
    const held = MODEL.nodes.find((node) => node.id !== MODEL.focusId)!.id;
    const anchor = animated.anchorOf(held);
    for (const sim of [animated, reduced]) {
      for (let i = 0; i < 90; i += 1) {
        sim.applyDrag(held, anchor.x - 140, anchor.y + 70);
        sim.step(DT);
      }
      sim.release();
    }
    const settledFrames = animated.settle({ dt: DT });
    for (let i = 0; i < settledFrames; i += 1) reduced.step(DT);
    expect(Array.from(reduced.positions())).toEqual(Array.from(animated.positions()));
    // Final positions must be the layout again: release returns the field home.
    for (const id of animated.nodeIds) {
      const home = animated.anchorOf(id);
      const now = animated.positionOf(id);
      expect(
        Math.hypot(now.x - home.x, now.y - home.y),
        `${id} settled away from its layout anchor`,
      ).toBeLessThan(1);
    }
  });

  it('exposes readback surfaces as copies, not live views into the integrator', () => {
    const sim = fieldSim(1);
    const snapshot = sim.positions();
    snapshot[0] = 1e9;
    sim.step(DT);
    expect(sim.positions()[0]).not.toBe(1e9);
    expect(sim.positionOf(MODEL.focusId).x).not.toBe(1e9);
    const params = sim.params;
    expect(() => {
      (params as { anchorBase: number }).anchorBase = 1;
    }).toThrow();
  });

  it('treats substepping as a refinement, not a different simulation', () => {
    // One 1/60 step must equal four 1/240 steps taken through the public API,
    // which is what lets the surface choose its frame budget without changing
    // feel.
    const coarse = fieldSim(8);
    const fine = fieldSim(8);
    coarse.step(1 / 60);
    for (let i = 0; i < 4; i += 1) fine.step(1 / 240);
    expect(Array.from(coarse.positions())).toEqual(Array.from(fine.positions()));
    expect(coarse.substepCount).toBe(fine.substepCount);
  });

  it('keeps the tuning defaults the prototype README documents', () => {
    expect({ ...DEFAULT_PARAMS }).toEqual({
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
    });
  });
});
