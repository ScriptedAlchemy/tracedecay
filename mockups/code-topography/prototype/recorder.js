/**
 * Flight recorder for the TRACE prototype.
 *
 * Round two exists to be measured, not admired. Every claim about feel in the
 * report — settle frames, follow ratios, frame-time p95 — comes out of a
 * recording made by this module, so a number in the README can be traced to a
 * captured frame rather than to an impression.
 *
 * The recorder is the only place in the prototype that reads a clock, and it
 * reads it for MEASUREMENT, never to drive the simulation: `frameMs` is the
 * wall-clock cost of producing a frame, while the simulation always advances by
 * its own fixed step. That separation is what makes a recording replayable.
 *
 * @module recorder
 */

/**
 * @param {object} options
 * @param {() => Float64Array} options.getPositions  flat [x,y,…] in node order
 * @param {string[]} options.nodeIds                 readback order, recorded once
 * @param {() => number} [options.now]               injectable clock, ms
 * @param {number} [options.maxFrames]               ring cap, drops nothing silently
 */
export function createRecorder({ getPositions, nodeIds, now, maxFrames = 20000 }) {
  if (typeof getPositions !== 'function') throw new TypeError('getPositions must be a function');
  if (!Array.isArray(nodeIds)) throw new TypeError('nodeIds must be an array');
  const clock = now ?? (() => (typeof performance === 'undefined' ? 0 : performance.now()));

  let recording = false;
  let startedAt = 0;
  let lastAt = 0;
  let dropped = 0;
  /** @type {Array<{t: number, frameMs: number, positions: number[]}>} */
  let frames = [];
  /** @type {Array<{t: number, frame: number, label: string, detail?: unknown}>} */
  let marks = [];

  return {
    get recording() {
      return recording;
    },
    get frameCount() {
      return frames.length;
    },

    /** Begin a take. Discards any previous one. */
    start(label = 'start') {
      frames = [];
      marks = [];
      dropped = 0;
      recording = true;
      startedAt = clock();
      lastAt = startedAt;
      marks.push({ t: 0, frame: 0, label });
      return this;
    },

    /** End the take and hand back the recording. */
    stop(label = 'stop') {
      if (recording) marks.push({ t: clock() - startedAt, frame: frames.length, label });
      recording = false;
      return this.toJSON();
    },

    /**
     * Capture one frame. Called from the page's animation loop AFTER the
     * simulation has stepped, so frame N holds the positions frame N drew.
     *
     * Two different times are recorded and they answer different questions:
     * `frameMs` is the INTERVAL since the previous captured frame — at a healthy
     * 60 Hz it is 16.7 ms by definition, so it detects dropped frames and
     * nothing else. `workMs` is the time the page spent stepping and painting,
     * which is the number that has to fit inside the budget. Conflating them
     * produces the false failure of a page that is running perfectly.
     *
     * @param {number} [workMs] measured cost of producing this frame.
     */
    capture(workMs) {
      if (!recording) return this;
      if (frames.length >= maxFrames) {
        dropped += 1;
        return this;
      }
      const at = clock();
      const positions = getPositions();
      const copy = new Array(positions.length);
      for (let i = 0; i < positions.length; i += 1) copy[i] = positions[i];
      frames.push({
        t: at - startedAt,
        frameMs: at - lastAt,
        workMs: typeof workMs === 'number' ? workMs : null,
        positions: copy,
      });
      lastAt = at;
      return this;
    },

    /** Annotate the timeline — gesture boundaries, theme flips, releases. */
    mark(label, detail) {
      if (!recording) return this;
      marks.push({ t: clock() - startedAt, frame: frames.length, label, detail });
      return this;
    },

    /**
     * The recording, plus the frame-time distribution the QA gate asserts on.
     * `droppedFrames` is reported rather than hidden: a truncated take must not
     * be able to masquerade as a complete one.
     */
    toJSON() {
      const distribution = (values) => {
        const sorted = values.filter((value) => typeof value === 'number').sort((a, b) => a - b);
        const quantile = (q) =>
          sorted.length === 0 ? null : sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
        return {
          p50: quantile(0.5),
          p95: quantile(0.95),
          max: sorted.length ? sorted[sorted.length - 1] : null,
        };
      };
      return {
        nodeIds: nodeIds.slice(),
        frameCount: frames.length,
        droppedFrames: dropped,
        durationMs: frames.length ? frames[frames.length - 1].t : 0,
        // The first interval is dropped: it is measured from `start()`, not from
        // a previous frame, so it is not a frame time at all.
        frameMs: distribution(frames.slice(1).map((frame) => frame.frameMs)),
        workMs: distribution(frames.map((frame) => frame.workMs)),
        marks: marks.slice(),
        frames,
      };
    },

    /** Serialised take, for `qa-drive.mjs` or a manual download. */
    toBlobUrl() {
      const json = JSON.stringify(this.toJSON());
      const blob = new Blob([json], { type: 'application/json' });
      return URL.createObjectURL(blob);
    },
  };
}

/**
 * Per-node displacement between two recorded frames, in world px.
 *
 * Lives beside the recorder rather than in the QA script because it is the
 * definition of "followed" that every assertion and every reported ratio
 * shares — one definition, so the gate and the report cannot drift.
 *
 * @param {{nodeIds: string[], frames: Array<{positions: number[]}>}} take
 * @param {number} fromFrame
 * @param {number} toFrame
 * @returns {Map<string, number>}
 */
export function displacements(take, fromFrame, toFrame) {
  const a = take.frames[fromFrame];
  const b = take.frames[toFrame];
  if (!a || !b) throw new RangeError(`frames ${fromFrame}..${toFrame} not in a take of ${take.frames.length}`);
  const out = new Map();
  take.nodeIds.forEach((id, i) => {
    out.set(id, Math.hypot(b.positions[i * 2] - a.positions[i * 2], b.positions[i * 2 + 1] - a.positions[i * 2 + 1]));
  });
  return out;
}

/**
 * First frame at or after `fromFrame` where every node moved less than
 * `restPx` between consecutive frames — the recorded definition of settled.
 *
 * @returns {number} frame index, or -1 if the take never settles.
 */
export function settleFrame(take, fromFrame, restPx = 0.01) {
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
