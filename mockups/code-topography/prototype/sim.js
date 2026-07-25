/**
 * Pure spring simulation for the TRACE surface.
 *
 * This module is the honesty boundary. Every felt quantity in the prototype —
 * weight, latency, deformation, settle time — is computed HERE from a stated
 * measurement, and the renderer only draws what comes out. Nothing in this file
 * touches the DOM, a canvas, `Date.now`, `performance.now` or `Math.random`, so
 * the same seed and the same gesture script produce a bit-identical trajectory
 * on every machine and inside `node --test`.
 *
 * Physics: hand-rolled position Verlet (Störmer–Verlet) with per-node velocity
 * damping, integrated at a fixed substep. Two force families:
 *
 *   anchor  every node is held to its watershed layout position by a spring of
 *           stiffness `anchorBase * mass^anchorMassExponent`. Because the
 *           anchor is the ONLY force at rest (see `restLength` below), the
 *           layout is an exact equilibrium: the live page at rest is the static
 *           sheet, pixel for pixel.
 *   channel every drawn `calls` edge is a spring whose stiffness is its
 *           call-site count. Rest length is the distance between the two
 *           layout anchors, so a channel stores zero energy at rest and only
 *           pulls once something has been displaced.
 *
 * Mass is the symbol's degree. With a uniform damping RATIO (not a uniform
 * damping coefficient) the closed form of the anchored oscillator gives
 * settle time ∝ mass^(anchorMassExponent/2 … ) — monotone in mass, which is
 * the "hubs are slow and deep, leaves flick" clause of the sensory contract,
 * and `sim.test.mjs` asserts the monotonicity rather than trusting the algebra.
 *
 * @module sim
 */

/**
 * Tuning defaults. Every one of these is a feel knob; the README carries the
 * table with the reasoning and the measured consequence of each.
 */
export const DEFAULT_PARAMS = Object.freeze({
  /** Anchor stiffness at unit mass, in force units per px. */
  anchorBase: 90,
  /**
   * Exponent applied to mass when scaling anchor stiffness. 0 would make every
   * node oscillate at its own natural frequency and a 63-degree hub would take
   * ~4.6x as long as a 3-degree leaf to settle — true to the measurement but
   * unusable as an interface. 1 would cancel mass out entirely and destroy the
   * weight channel. 0.5 keeps latency strictly monotone in degree with a ~2.1x
   * spread across this subgraph, which reads as weight without stalling.
   */
  anchorMassExponent: 0.5,
  /** Channel stiffness per call site, in force units per px. */
  edgeStiffnessScale: 6,
  /**
   * Damping ratio of the anchored oscillator. Below 1 is underdamped. 0.72
   * gives one small overshoot — flesh, not jelly — and no ringing.
   */
  dampingRatio: 0.72,
  /** Integrator substep, in seconds. `step(dt)` subdivides down to this. */
  substep: 1 / 240,
  /** Speed below which a node counts as at rest, in px/s. */
  restSpeed: 0.6,
  /** Degree floor, so an unresolved (degree 0) node still has inertia. */
  minMass: 3,
  /** Amplitude of the seeded startup displacement, in px. */
  jitter: 6,
  /** Hover-bloom approach rate at unit mass, in 1/s, growing. */
  bloomAttack: 9.5,
  /** Hover-bloom approach rate at unit mass, in 1/s, decaying. */
  bloomRelease: 5,
  /** Exponent applied to mass when slowing the bloom approach. */
  bloomMassExponent: 0.42,
});

/**
 * Deterministic PRNG (mulberry32). Used ONCE, at construction, to break the
 * perfect symmetry of the layout so the field visibly breathes into place.
 * Never called during stepping — that is what makes replay exact.
 */
function mulberry32(seed) {
  let a = seed >>> 0;
  return function next() {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function requireFinite(value, what) {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new TypeError(`${what} must be a finite number, got ${String(value)}`);
  }
  return value;
}

/**
 * Build a simulation over a measured subgraph.
 *
 * @param {object} spec
 * @param {Array<{id: string, mass: number, x0: number, y0: number}>} spec.nodes
 *   `mass` is the symbol's degree; `x0`/`y0` are its watershed layout anchor.
 * @param {Array<{a: string, b: string, stiffness: number, restLength?: number}>} spec.springs
 *   `stiffness` is the edge's call-site count. `restLength` defaults to the
 *   distance between the two anchors, which makes the layout an equilibrium.
 * @param {number} [spec.seed] seed for the startup displacement only.
 * @param {Partial<typeof DEFAULT_PARAMS>} [spec.params]
 */
export function createSimulation(spec) {
  if (!spec || !Array.isArray(spec.nodes) || !Array.isArray(spec.springs)) {
    throw new TypeError('createSimulation needs { nodes: [], springs: [] }');
  }
  const params = { ...DEFAULT_PARAMS, ...(spec.params ?? {}) };
  requireFinite(params.substep, 'params.substep');
  if (params.substep <= 0) throw new RangeError('params.substep must be > 0');

  const count = spec.nodes.length;
  if (count === 0) throw new RangeError('createSimulation needs at least one node');

  const ids = new Array(count);
  const indexOfId = new Map();
  const mass = new Float64Array(count);
  const invMass = new Float64Array(count);
  const anchorX = new Float64Array(count);
  const anchorY = new Float64Array(count);
  const anchorK = new Float64Array(count);
  const dampPerSubstep = new Float64Array(count);
  const x = new Float64Array(count);
  const y = new Float64Array(count);
  const prevX = new Float64Array(count);
  const prevY = new Float64Array(count);
  const forceX = new Float64Array(count);
  const forceY = new Float64Array(count);
  const pinned = new Uint8Array(count);
  const pinX = new Float64Array(count);
  const pinY = new Float64Array(count);

  const random = mulberry32(requireFinite(spec.seed ?? 1, 'spec.seed'));

  spec.nodes.forEach((node, i) => {
    if (!node || typeof node.id !== 'string' || node.id.length === 0) {
      throw new TypeError(`nodes[${i}].id must be a non-empty string`);
    }
    if (indexOfId.has(node.id)) throw new TypeError(`duplicate node id ${node.id}`);
    indexOfId.set(node.id, i);
    ids[i] = node.id;
    const m = Math.max(params.minMass, requireFinite(node.mass, `nodes[${i}].mass`));
    mass[i] = m;
    invMass[i] = 1 / m;
    anchorX[i] = requireFinite(node.x0, `nodes[${i}].x0`);
    anchorY[i] = requireFinite(node.y0, `nodes[${i}].y0`);
    anchorK[i] = params.anchorBase * Math.pow(m, params.anchorMassExponent);
    // c = 2 ζ sqrt(k m) is the damping coefficient that realises the target
    // ratio against this node's own anchor spring. Applying it as a
    // multiplicative velocity factor per substep (exp of the decay rate) means
    // kinetic energy can only ever go DOWN in the damping half of the step,
    // which is what keeps the post-release energy curve monotone.
    const c = 2 * params.dampingRatio * Math.sqrt(anchorK[i] * m);
    dampPerSubstep[i] = Math.exp((-c / m) * params.substep);
  });

  // Seeded startup displacement, applied as an offset with zero initial
  // velocity (prev === current), so the field falls into the layout.
  for (let i = 0; i < count; i += 1) {
    const angle = random() * Math.PI * 2;
    const radius = params.jitter * (0.35 + random() * 0.65);
    x[i] = anchorX[i] + Math.cos(angle) * radius;
    y[i] = anchorY[i] + Math.sin(angle) * radius;
    prevX[i] = x[i];
    prevY[i] = y[i];
  }

  const springCount = spec.springs.length;
  const springA = new Int32Array(springCount);
  const springB = new Int32Array(springCount);
  const springK = new Float64Array(springCount);
  const springRest = new Float64Array(springCount);
  /** @type {Map<string, Array<{spring: number, other: number}>>} */
  const adjacency = new Map();
  ids.forEach((id) => adjacency.set(id, []));

  spec.springs.forEach((spring, i) => {
    const a = indexOfId.get(spring?.a);
    const b = indexOfId.get(spring?.b);
    if (a === undefined) throw new TypeError(`springs[${i}].a references unknown node ${String(spring?.a)}`);
    if (b === undefined) throw new TypeError(`springs[${i}].b references unknown node ${String(spring?.b)}`);
    if (a === b) throw new TypeError(`springs[${i}] is a self-loop on ${ids[a]}`);
    const k = requireFinite(spring.stiffness, `springs[${i}].stiffness`);
    if (k <= 0) throw new RangeError(`springs[${i}].stiffness must be > 0`);
    springA[i] = a;
    springB[i] = b;
    springK[i] = k * params.edgeStiffnessScale;
    springRest[i] =
      spring.restLength === undefined
        ? Math.hypot(anchorX[b] - anchorX[a], anchorY[b] - anchorY[a])
        : requireFinite(spring.restLength, `springs[${i}].restLength`);
    adjacency.get(ids[a]).push({ spring: i, other: ids[b] });
    adjacency.get(ids[b]).push({ spring: i, other: ids[a] });
  });

  let stepCount = 0;
  let substepCount = 0;

  function accumulateForces() {
    for (let i = 0; i < count; i += 1) {
      forceX[i] = -anchorK[i] * (x[i] - anchorX[i]);
      forceY[i] = -anchorK[i] * (y[i] - anchorY[i]);
    }
    for (let s = 0; s < springCount; s += 1) {
      const a = springA[s];
      const b = springB[s];
      const dx = x[b] - x[a];
      const dy = y[b] - y[a];
      const length = Math.hypot(dx, dy);
      if (length === 0) continue;
      const magnitude = springK[s] * (length - springRest[s]);
      const fx = (dx / length) * magnitude;
      const fy = (dy / length) * magnitude;
      forceX[a] += fx;
      forceY[a] += fy;
      forceX[b] -= fx;
      forceY[b] -= fy;
    }
  }

  function integrate(dt) {
    accumulateForces();
    const dt2 = dt * dt;
    for (let i = 0; i < count; i += 1) {
      if (pinned[i]) {
        // A pinned node is the pointer, not a body: it goes exactly where the
        // gesture says and carries no velocity of its own. Its neighbours feel
        // it only through the channel springs.
        x[i] = pinX[i];
        y[i] = pinY[i];
        prevX[i] = pinX[i];
        prevY[i] = pinY[i];
        continue;
      }
      const damp = dampPerSubstep[i];
      const stepX = (x[i] - prevX[i]) * damp + forceX[i] * invMass[i] * dt2;
      const stepY = (y[i] - prevY[i]) * damp + forceY[i] * invMass[i] * dt2;
      prevX[i] = x[i];
      prevY[i] = y[i];
      x[i] += stepX;
      y[i] += stepY;
    }
    substepCount += 1;
  }

  function resolveIndex(nodeId) {
    const i = indexOfId.get(nodeId);
    if (i === undefined) throw new TypeError(`unknown node id ${String(nodeId)}`);
    return i;
  }

  const sim = {
    /** Node ids in readback order. */
    get nodeIds() {
      return ids.slice();
    },
    get nodeCount() {
      return count;
    },
    get springCount() {
      return springCount;
    },
    /** Frozen copy of the parameters actually in force. */
    get params() {
      return Object.freeze({ ...params });
    },
    get stepCount() {
      return stepCount;
    },
    get substepCount() {
      return substepCount;
    },

    indexOf: resolveIndex,
    massOf(nodeId) {
      return mass[resolveIndex(nodeId)];
    },
    anchorOf(nodeId) {
      const i = resolveIndex(nodeId);
      return { x: anchorX[i], y: anchorY[i] };
    },
    /** Channel stiffnesses incident on a node, keyed by the other endpoint. */
    springsOf(nodeId) {
      resolveIndex(nodeId);
      return adjacency.get(nodeId).map((entry) => ({
        other: entry.other,
        stiffness: springK[entry.spring],
        restLength: springRest[entry.spring],
      }));
    },

    /**
     * Advance by `dt` seconds, subdivided into equal substeps no larger than
     * `params.substep`. Callers MUST pass a fixed `dt` (the page drives one
     * fixed step per animation frame) — wall-clock jitter never reaches the
     * integrator, which is the whole reason a recording can be replayed.
     */
    step(dt) {
      requireFinite(dt, 'dt');
      if (dt <= 0) throw new RangeError('dt must be > 0');
      const substeps = Math.max(1, Math.ceil(dt / params.substep - 1e-9));
      const h = dt / substeps;
      for (let s = 0; s < substeps; s += 1) integrate(h);
      stepCount += 1;
      return this;
    },

    /** Pin a node to a gesture position. Idempotent per frame. */
    applyDrag(nodeId, targetX, targetY) {
      const i = resolveIndex(nodeId);
      pinned[i] = 1;
      pinX[i] = requireFinite(targetX, 'targetX');
      pinY[i] = requireFinite(targetY, 'targetY');
      return this;
    },

    /** Release every pinned node. Velocity stays zero — no fling. */
    release() {
      pinned.fill(0);
      return this;
    },

    /** @returns {string[]} ids currently pinned. */
    pinnedIds() {
      const out = [];
      for (let i = 0; i < count; i += 1) if (pinned[i]) out.push(ids[i]);
      return out;
    },

    /** Flat [x0,y0,x1,y1,…] copy in `nodeIds` order. */
    positions() {
      const out = new Float64Array(count * 2);
      for (let i = 0; i < count; i += 1) {
        out[i * 2] = x[i];
        out[i * 2 + 1] = y[i];
      }
      return out;
    },
    positionOf(nodeId) {
      const i = resolveIndex(nodeId);
      return { x: x[i], y: y[i] };
    },
    /** Flat [vx0,vy0,…] in px/s, derived from the Verlet position history. */
    velocities() {
      const out = new Float64Array(count * 2);
      const inv = 1 / params.substep;
      for (let i = 0; i < count; i += 1) {
        out[i * 2] = (x[i] - prevX[i]) * inv;
        out[i * 2 + 1] = (y[i] - prevY[i]) * inv;
      }
      return out;
    },
    maxSpeed() {
      const inv = 1 / params.substep;
      let worst = 0;
      for (let i = 0; i < count; i += 1) {
        if (pinned[i]) continue;
        const speed = Math.hypot(x[i] - prevX[i], y[i] - prevY[i]) * inv;
        if (speed > worst) worst = speed;
      }
      return worst;
    },
    /**
     * Kinetic + anchor + channel energy. With every node damped and no node
     * pinned, this can only fall; `sim.test.mjs` asserts exactly that.
     */
    energy() {
      const inv = 1 / params.substep;
      let kinetic = 0;
      let anchor = 0;
      for (let i = 0; i < count; i += 1) {
        const vx = (x[i] - prevX[i]) * inv;
        const vy = (y[i] - prevY[i]) * inv;
        kinetic += 0.5 * mass[i] * (vx * vx + vy * vy);
        const dx = x[i] - anchorX[i];
        const dy = y[i] - anchorY[i];
        anchor += 0.5 * anchorK[i] * (dx * dx + dy * dy);
      }
      let channel = 0;
      for (let s = 0; s < springCount; s += 1) {
        const stretch =
          Math.hypot(x[springB[s]] - x[springA[s]], y[springB[s]] - y[springA[s]]) - springRest[s];
        channel += 0.5 * springK[s] * stretch * stretch;
      }
      return { kinetic, anchor, channel, total: kinetic + anchor + channel };
    },
    /** Signed stretch per spring, in px: what the renderer draws as tension. */
    stretches() {
      const out = new Float64Array(springCount);
      for (let s = 0; s < springCount; s += 1) {
        out[s] =
          Math.hypot(x[springB[s]] - x[springA[s]], y[springB[s]] - y[springA[s]]) - springRest[s];
      }
      return out;
    },
    isSettled(restSpeed = params.restSpeed) {
      return sim.maxSpeed() < restSpeed;
    },
    /**
     * Run the SAME `step()` the animated loop runs until the field is at rest.
     * The reduced-motion mode is exactly this: identical arithmetic, drawn once
     * instead of sixty times a second. That identity is the a11y guarantee, and
     * it is asserted, not asserted-in-prose.
     */
    settle({ dt = 1 / 60, maxFrames = 1200, restSpeed = params.restSpeed } = {}) {
      let frames = 0;
      while (frames < maxFrames) {
        sim.step(dt);
        frames += 1;
        if (sim.maxSpeed() < restSpeed) break;
      }
      return frames;
    },
  };

  return sim;
}

/**
 * Hop distance over the channel graph, ignoring direction. Used by the QA
 * harness to say what "far" means when it asserts that far nodes did not move.
 *
 * @returns {Map<string, number>} id → hops, unreachable ids omitted.
 */
export function hopDistances(sim, sourceId) {
  const seen = new Map([[sourceId, 0]]);
  let frontier = [sourceId];
  while (frontier.length) {
    const next = [];
    for (const id of frontier) {
      const hops = seen.get(id) + 1;
      for (const { other } of sim.springsOf(id)) {
        if (seen.has(other)) continue;
        seen.set(other, hops);
        next.push(other);
      }
    }
    frontier = next;
  }
  return seen;
}

/**
 * One exponential-approach step of the hover bloom.
 *
 * Bloom is not physics — it is the hover channel of the sensory contract, and
 * it lives here so it is testable and lifts with the simulation rather than
 * with a renderer. The approach RATE is divided by mass, so a leaf snaps and a
 * hub arrives late and keeps arriving: "hover-response latency scales with
 * degree", drawn from the same number that sets the node's inertia.
 *
 * @param {number} current  bloom in [0,1]
 * @param {number} target   0 or 1
 * @param {number} mass     the node's degree
 */
export function bloomStep(current, target, mass, dt, params = DEFAULT_PARAMS) {
  const base = target > current ? params.bloomAttack : params.bloomRelease;
  const rate = base / Math.pow(Math.max(params.minMass, mass), params.bloomMassExponent);
  return current + (target - current) * (1 - Math.exp(-rate * dt));
}
