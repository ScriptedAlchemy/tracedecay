/**
 * Canvas2D renderer for the TRACE surface.
 *
 * This module draws and does nothing else. It is handed positions, per-channel
 * stretch, per-node bloom and a resolved palette, and it paints one frame. It
 * never integrates, never decides how a body responds to a gesture, and never
 * reads the clock — if a mark moves, it is because the simulation moved it.
 *
 * Canvas cannot read CSS custom properties, so the composing page resolves the
 * token block once per theme flip and hands the resolved strings in. That keeps
 * `tokens.css` the single source of the instrument's colour without putting a
 * `getComputedStyle` call inside the draw loop.
 *
 * @module render
 */

/* ---- kind hue: verbatim transcription of dashboard/src/viz/graph/kindColor.ts */

function hashKind(kind) {
  let hash = 0;
  for (let index = 0; index < kind.length; index += 1) {
    hash = (hash * 31 + kind.charCodeAt(index)) >>> 0;
  }
  return hash;
}

/** @param light whether the kind is drawn against a light medium. */
export function kindColor(kind, light) {
  const hash = hashKind(kind);
  const chroma = (light ? 0.135 : 0.152) + ((hash >>> 9) % 6) * 0.012;
  const lightness = light ? 0.55 : 0.72;
  return `oklch(${lightness} ${chroma.toFixed(3)} ${186 + (hash % 148)})`;
}

/* ---- deterministic relief noise (shared with the static sheets) ---------- */

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

/**
 * A closed relief outline. `rough` is the region's own irregularity, handed in
 * by the caller — a lumpy coastline means a lumpy module.
 */
function blob(ctx, cx, cy, radiusX, radiusY, seed, rough = 0.14, samples = 72) {
  const random = mulberry32(seed);
  const harmonics = [
    { k: 2 + Math.floor(random() * 2), a: rough * (0.55 + random() * 0.5), p: random() * 6.283 },
    { k: 3 + Math.floor(random() * 3), a: rough * (0.34 + random() * 0.4), p: random() * 6.283 },
    { k: 6 + Math.floor(random() * 4), a: rough * (0.14 + random() * 0.2), p: random() * 6.283 },
  ];
  ctx.beginPath();
  for (let i = 0; i <= samples; i += 1) {
    const t = (i / samples) * Math.PI * 2;
    let m = 1;
    for (const h of harmonics) m += h.a * Math.sin(h.k * t + h.p);
    const px = cx + Math.cos(t) * radiusX * m;
    const py = cy + Math.sin(t) * radiusY * m;
    if (i === 0) ctx.moveTo(px, py);
    else ctx.lineTo(px, py);
  }
  ctx.closePath();
}

/**
 * Catmull–Rom through the waypoints, emitted as cubics. This is what makes
 * several channels sharing a trunk read as one braided river rather than as
 * parallel arrows.
 */
function traceCurve(ctx, points, move) {
  const p = [points[0], ...points, points[points.length - 1]];
  if (move) ctx.moveTo(p[1][0], p[1][1]);
  else ctx.lineTo(p[1][0], p[1][1]);
  for (let i = 1; i < p.length - 2; i += 1) {
    const [x0, y0] = p[i - 1];
    const [x1, y1] = p[i];
    const [x2, y2] = p[i + 1];
    const [x3, y3] = p[i + 2];
    ctx.bezierCurveTo(
      x1 + (x2 - x0) / 6,
      y1 + (y2 - y0) / 6,
      x2 - (x3 - x1) / 6,
      y2 - (y3 - y1) / 6,
      x2,
      y2,
    );
  }
}

/** A tapered ribbon: width is a measured quantity at each end. */
function ribbon(ctx, points, widthStart, widthEnd) {
  const n = points.length;
  const normals = points.map((_, i) => {
    const a = points[Math.max(0, i - 1)];
    const b = points[Math.min(n - 1, i + 1)];
    const dx = b[0] - a[0];
    const dy = b[1] - a[1];
    const len = Math.hypot(dx, dy) || 1;
    return [-dy / len, dx / len];
  });
  const half = (i) => (widthStart + (widthEnd - widthStart) * (i / (n - 1))) / 2;
  const upper = points.map((pt, i) => [pt[0] + normals[i][0] * half(i), pt[1] + normals[i][1] * half(i)]);
  const lower = points
    .map((pt, i) => [pt[0] - normals[i][0] * half(i), pt[1] - normals[i][1] * half(i)])
    .reverse();
  ctx.beginPath();
  traceCurve(ctx, upper, true);
  traceCurve(ctx, lower, false);
  ctx.closePath();
}

function roundRect(ctx, x, y, w, h, r) {
  const radius = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + radius, y);
  ctx.arcTo(x + w, y, x + w, y + h, radius);
  ctx.arcTo(x + w, y + h, x, y + h, radius);
  ctx.arcTo(x, y + h, x, y, radius);
  ctx.arcTo(x, y, x + w, y, radius);
  ctx.closePath();
}

/* ---- measurement → mark ------------------------------------------------- */

/** Channel width in px: the call-site count on that one edge. */
export function channelWidth(calls) {
  return 2.2 + Math.sqrt(calls) * 1.15;
}

/** Node sill width in px: the symbol's degree, straight off the payload. */
export function sillWidth(degree) {
  return 16 + degree * 0.62;
}

/** Below this stretch a channel is at rest and gets no tension rail. */
const TENSION_FLOOR_PX = 5;
/** Stretch at which the tension rail reaches full thickness. */
const TENSION_SATURATION_PX = 60;

const HOP_LABEL = [
  ['u3', '3 hops up'],
  ['u2', '2 hops up'],
  ['u1', '1 hop up'],
  ['focus', 'focus'],
  ['d1', '1 hop down'],
  ['d2', '2 hops down'],
  ['d3', '3 hops down'],
];

/**
 * @param {HTMLCanvasElement} canvas
 * @param {import('./dataset.js').DATASET} dataset
 */
export function createRenderer(canvas, dataset) {
  const ctx = canvas.getContext('2d', { alpha: true });
  if (!ctx) throw new Error('Canvas2D unavailable');

  const nodes = dataset.nodes;
  const indexOfId = new Map(nodes.map((node, i) => [node.id, i]));
  /** Region seeds are stable per module name, so the coastline never shimmers. */
  const regionSeed = dataset.regions.map((region) => hashKind(region.mod));

  let palette = null;
  let view = { width: 0, height: 0, dpr: 1, scale: 1, offsetX: 0, offsetY: 0 };

  /** Cheap per-frame scratch so a 60 Hz loop allocates nothing. */
  const point = { x: 0, y: 0 };
  function readPosition(positions, i) {
    point.x = positions[i * 2];
    point.y = positions[i * 2 + 1];
    return point;
  }

  function label(text, x, y, { size = 9, color, align = 'left', tracking = 0, halo = true, upper = false } = {}) {
    const body = upper ? text.toUpperCase() : text;
    ctx.font = `${size}px ui-monospace, "SFMono-Regular", "JetBrains Mono", monospace`;
    ctx.textAlign = tracking > 0 ? 'left' : align;
    ctx.textBaseline = 'alphabetic';
    // A label on a moving field has to survive every crossing, so it is set
    // with a substrate-coloured halo exactly as the static sheets do it.
    if (tracking > 0) {
      const glyphs = [...body];
      const total = glyphs.reduce((sum, g) => sum + ctx.measureText(g).width + tracking, -tracking);
      let cursor = align === 'right' ? x - total : align === 'center' ? x - total / 2 : x;
      for (const glyph of glyphs) {
        if (halo) {
          ctx.strokeStyle = palette.surface0;
          ctx.lineWidth = 3;
          ctx.strokeText(glyph, cursor, y);
        }
        ctx.fillStyle = color ?? palette.textMuted;
        ctx.fillText(glyph, cursor, y);
        cursor += ctx.measureText(glyph).width + tracking;
      }
      return;
    }
    if (halo) {
      ctx.strokeStyle = palette.surface0;
      ctx.lineWidth = 3;
      ctx.lineJoin = 'round';
      ctx.strokeText(body, x, y);
    }
    ctx.fillStyle = color ?? palette.textMuted;
    ctx.fillText(body, x, y);
  }

  /* ---- 1. underlay: the dimmed relief, moving with its members ----------- */
  function drawUnderlay(positions) {
    ctx.save();
    ctx.globalAlpha = 0.22;
    const centres = dataset.regions.map((region, r) => {
      let cx = 0;
      let cy = 0;
      for (const id of region.of) {
        const p = readPosition(positions, indexOfId.get(id));
        cx += p.x;
        cy += p.y;
      }
      cx /= region.of.length;
      cy /= region.of.length;
      let spreadX = 0;
      let spreadY = 0;
      for (const id of region.of) {
        const p = readPosition(positions, indexOfId.get(id));
        spreadX = Math.max(spreadX, Math.abs(p.x - cx));
        spreadY = Math.max(spreadY, Math.abs(p.y - cy));
      }
      const mass = Math.sqrt(region.sym) * 3.4;
      const rx = spreadX + mass;
      const ry = spreadY + mass * 0.62;
      blob(ctx, cx, cy, rx, ry, regionSeed[r], 0.11);
      ctx.fillStyle = palette.reliefFillLo;
      ctx.fill();
      ctx.strokeStyle = palette.reliefLineIndex;
      ctx.lineWidth = 1.6;
      ctx.stroke();
      blob(ctx, cx, cy, rx * 0.6, ry * 0.6, regionSeed[r] + 11, 0.09);
      ctx.strokeStyle = palette.reliefLine;
      ctx.lineWidth = 0.8;
      ctx.stroke();
      return { cx, cy, rx, ry };
    });
    ctx.restore();
    // The underlay is dim; its labels are not. A boundary you cannot name is
    // decoration, and nothing on this page is allowed to be decoration.
    dataset.regions.forEach((region, r) => {
      const c = centres[r];
      label(`${region.mod}/`, c.cx - c.rx * 0.62, c.cy + c.ry * 0.94, {
        color: palette.textMuted,
        tracking: 1.1,
        upper: true,
      });
    });
  }

  /* ---- 2. membranes: the types the flow passes through ------------------- */
  function drawMembranes(positions) {
    for (const membrane of dataset.membranes) {
      let x0 = Infinity;
      let x1 = -Infinity;
      let y0 = Infinity;
      let y1 = -Infinity;
      for (const id of membrane.of) {
        const p = readPosition(positions, indexOfId.get(id));
        x0 = Math.min(x0, p.x);
        x1 = Math.max(x1, p.x);
        y0 = Math.min(y0, p.y);
        y1 = Math.max(y1, p.y);
      }
      x0 -= 64;
      x1 += 64;
      y0 -= 54;
      y1 += 54;
      ctx.save();
      roundRect(ctx, x0, y0, x1 - x0, y1 - y0, Math.min((y1 - y0) / 2, 74));
      ctx.fillStyle = membrane.hero ? palette.membraneHero : palette.membraneFill;
      ctx.fill();
      ctx.strokeStyle = membrane.hero ? palette.accent : palette.edgeStrong;
      ctx.lineWidth = membrane.hero ? 1.4 : 1;
      ctx.globalAlpha = membrane.hero ? 0.8 : 0.7;
      // A trait's enclosure is dashed because the concrete side is chosen at
      // run time; an impl's is solid because it is not.
      if (membrane.kind === 'trait') ctx.setLineDash([5, 4]);
      ctx.stroke();
      ctx.restore();
      label(membrane.label, x0 + 12, y0 - 7, {
        color: membrane.hero ? palette.accent : palette.textMuted,
        tracking: 1.1,
        upper: true,
      });
      label(membrane.file.split('/').slice(-2).join('/'), x1 - 12, y0 - 7, {
        color: palette.textMuted,
        align: 'right',
      });
      membrane.box = { x0, y0, x1, y1 };
    }
  }

  /* ---- 3. channels: width is call sites, brightness is live tension ------ */
  function drawChannels(positions, stretches, reducedMotion) {
    const focusIndex = indexOfId.get(dataset.focusId);
    const focus = { x: positions[focusIndex * 2], y: positions[focusIndex * 2 + 1] };
    const readings = [];
    dataset.edges.forEach((edge, e) => {
      const ai = indexOfId.get(edge.a);
      const bi = indexOfId.get(edge.b);
      const ax = positions[ai * 2];
      const ay = positions[ai * 2 + 1];
      const bx = positions[bi * 2];
      const by = positions[bi * 2 + 1];
      const w = channelWidth(edge.calls);
      let points;
      if (edge.dir === 'in') {
        // A call that entered a type and moves between its methods before
        // leaving. Drawn, not implied — it is the sheet's whole argument.
        points = [
          [ax, ay],
          [(ax + bx) / 2, Math.min(ay, by) - 46],
          [bx, by],
        ];
      } else {
        const mid = (ay + by) / 2;
        const pull = edge.dir === 'up' ? 0.42 : 0.34;
        points = [
          [ax, ay],
          [ax + (focus.x - ax) * pull * 0.5, mid - (by - ay) * 0.18],
          [bx + (focus.x - bx) * pull * 0.12, mid + (by - ay) * 0.2],
          [bx, by],
        ];
      }
      const upstream = edge.dir === 'up' || edge.dir === 'in';
      const hue = upstream ? palette.upstream : palette.downstream;
      const widthStart = edge.dir === 'up' ? w * 0.78 : w;
      const widthEnd = edge.dir === 'up' ? w : w * 0.78;
      const stretch = Math.abs(stretches[e]);
      // Tension is the SAME measurement as width (call sites); under load the
      // channel reports how far it has been pulled off its rest length, so the
      // felt channel and the drawn channel cannot disagree.
      const load = Math.min(1, stretch / 26);
      ctx.save();
      ribbon(ctx, points, widthStart, widthEnd);
      ctx.fillStyle = hue;
      ctx.globalAlpha = edge.dir === 'lost' ? 0.18 : 0.42 + load * 0.3;
      ctx.fill();
      ctx.globalAlpha = edge.dir === 'lost' ? 0.5 : 0.62 + load * 0.38;
      ctx.strokeStyle = hue;
      ctx.lineWidth = 0.8 + load * 1.4;
      if (edge.dir === 'lost') ctx.setLineDash([4, 4]);
      ctx.stroke();
      ctx.restore();

      if (reducedMotion && stretch > TENSION_FLOOR_PX) {
        // Reduced motion: tension becomes literal thickness on a core rail,
        // because a reader who cannot see the deformation must still be able to
        // measure it. Two decisions here were forced by looking at the frame:
        // the rail saturates (a 172 px stretch drawn at true scale is a 23 px
        // slab that swamps the field), and it wears the PARTIAL state hue rather
        // than the accent, because an accent-coloured rail was indistinguishable
        // from the upstream channel it was sitting inside.
        const railWidth = 1.5 + Math.min(1, stretch / TENSION_SATURATION_PX) * 6.5;
        ctx.save();
        ribbon(ctx, points, railWidth, railWidth);
        ctx.fillStyle = palette.statePartial;
        ctx.globalAlpha = 0.72;
        ctx.fill();
        ctx.restore();
        readings.push({
          x: (points[0][0] + points[points.length - 1][0]) / 2,
          y: (ay + by) / 2,
          text: `${stretch.toFixed(0)} px`,
          hue: palette.statePartial,
        });
      } else if (edge.calls >= 9 || edge.dir === 'lost') {
        readings.push({
          x: (points[0][0] + points[points.length - 1][0]) / 2,
          y: (ay + by) / 2 + (edge.dir === 'in' ? -30 : 0),
          text: String(edge.calls),
          hue,
        });
      }
    });
    return readings;
  }

  /* ---- 4. membrane ports: where flow crosses a type boundary ------------- */
  function drawPorts(positions) {
    for (const membrane of dataset.membranes) {
      const box = membrane.box;
      if (!box) continue;
      ctx.save();
      ctx.strokeStyle = membrane.hero ? palette.accent : palette.edgeStrong;
      ctx.lineWidth = 2.4;
      for (const id of membrane.of) {
        const p = readPosition(positions, indexOfId.get(id));
        for (const edgeY of [box.y0, box.y1]) {
          ctx.beginPath();
          ctx.moveTo(p.x - 9, edgeY);
          ctx.lineTo(p.x + 9, edgeY);
          ctx.stroke();
        }
      }
      ctx.restore();
    }
  }

  /* ---- 5. hover bloom: depth and latency are the node's degree ----------- */
  function drawBloom(positions, bloom, light) {
    nodes.forEach((node, i) => {
      const value = bloom[i];
      if (value < 0.004) return;
      const p = readPosition(positions, i);
      const hue = node.unresolved ? palette.stateUnknown : kindColor(node.kind, light);
      // Bloom DEPTH scales with degree: a hub opens a wide slow well, a leaf a
      // tight flick. Same number that sets its inertia.
      const reach = (14 + Math.max(3, node.deg) * 0.9) * value;
      ctx.save();
      for (let ring = 3; ring >= 1; ring -= 1) {
        ctx.beginPath();
        ctx.ellipse(p.x, p.y, sillWidth(node.deg) / 2 + reach * ring * 0.55, 7 + reach * ring * 0.34, 0, 0, Math.PI * 2);
        ctx.strokeStyle = hue;
        ctx.globalAlpha = value * (0.42 - ring * 0.09);
        ctx.lineWidth = ring === 1 ? 1.6 : 0.9;
        ctx.stroke();
      }
      ctx.restore();
    });
  }

  /* ---- 6. nodes: a sill whose WIDTH is the symbol's degree --------------- */
  function drawNodes(positions, light, draggingId) {
    nodes.forEach((node, i) => {
      if (node.id === dataset.focusId) return;
      const p = readPosition(positions, i);
      if (node.unresolved) {
        ctx.save();
        roundRect(ctx, p.x - 34, p.y - 5, 68, 10, 5);
        ctx.strokeStyle = palette.textMuted;
        ctx.lineWidth = 1;
        ctx.globalAlpha = 0.55;
        ctx.setLineDash([3, 4]);
        ctx.stroke();
        ctx.restore();
        return;
      }
      const w = sillWidth(node.deg);
      roundRect(ctx, p.x - w / 2, p.y - 5, w, 10, 5);
      ctx.save();
      ctx.fillStyle = kindColor(node.kind, light);
      ctx.globalAlpha = 0.9;
      ctx.fill();
      ctx.restore();
      ctx.strokeStyle = node.id === draggingId ? palette.accent : palette.surface0;
      ctx.lineWidth = node.id === draggingId ? 1.8 : 1;
      ctx.stroke();
    });
  }

  /* ---- 7. the focus basin ------------------------------------------------ */
  function drawFocus(positions) {
    const i = indexOfId.get(dataset.focusId);
    const p = readPosition(positions, i);
    ctx.save();
    for (let r = 4; r >= 1; r -= 1) {
      blob(ctx, p.x, p.y, 20 + r * 15, 12 + r * 8, 77 + r * 13, 0.07);
      if (r === 1) {
        ctx.fillStyle = palette.accent;
        ctx.globalAlpha = 0.85;
        ctx.fill();
      }
      ctx.strokeStyle = palette.accent;
      ctx.globalAlpha = 0.25 + (5 - r) * 0.16;
      ctx.lineWidth = r === 1 ? 1.6 : 0.9;
      ctx.stroke();
    }
    ctx.restore();
  }

  /* ---- 8. hop rings and labels, set last -------------------------------- */
  function drawRings() {
    ctx.save();
    for (const [key, text] of HOP_LABEL) {
      const y = dataset.row[key];
      ctx.beginPath();
      ctx.moveTo(96, y);
      ctx.lineTo(1416, y);
      ctx.strokeStyle = palette.grid;
      ctx.lineWidth = 1;
      ctx.globalAlpha = key === 'focus' ? 0.9 : 1;
      ctx.setLineDash(key === 'focus' ? [] : [1, 6]);
      ctx.stroke();
      ctx.setLineDash([]);
      label(text, 88, y + 3, {
        align: 'right',
        color: key === 'focus' ? palette.accent : palette.textMuted,
        tracking: 1.1,
        upper: true,
      });
    }
    ctx.restore();
    label('hop ring ↓', 88, 40, { align: 'right', tracking: 1.1, upper: true });
    label('depth limit 3 — 41 further symbols exist beyond this frame and are not drawn', 1416, 40, {
      align: 'right',
      tracking: 1.1,
      upper: true,
    });
  }

  function drawLabels(positions, readings) {
    nodes.forEach((node, i) => {
      if (node.id === dataset.focusId) return;
      const p = readPosition(positions, i);
      const above = node.y0 <= dataset.row.focus;
      const dy = above ? -14 : 22;
      if (node.unresolved) {
        label(node.name, p.x, p.y + dy, { size: 11, color: palette.textMuted, align: 'center' });
        label(node.unresolved, p.x, p.y + dy + 12, { color: palette.stateUnknown, align: 'center' });
        label('channel ends — target not in graph', p.x, p.y + dy - 12, {
          color: palette.stateUnknown,
          align: 'center',
          tracking: 1.1,
          upper: true,
        });
        return;
      }
      label(node.name, p.x, p.y + dy, { size: 11, color: palette.textPrimary, align: 'center' });
      label(`${node.mod}/ · deg ${node.deg}`, p.x, p.y + dy + (above ? -11 : 11), {
        color: palette.textMuted,
        align: 'center',
      });
      if (node.source) {
        label(node.source, p.x, p.y + dy - 22, {
          color: palette.statePartial,
          align: 'center',
          tracking: 1.1,
          upper: true,
        });
      }
    });

    const fi = indexOfId.get(dataset.focusId);
    const f = readPosition(positions, fi);
    const focusNode = nodes[fi];
    label(focusNode.name, f.x, f.y + 4, { size: 15, color: palette.textPrimary, align: 'center' });
    label(dataset.readout.focusStats, f.x, f.y + 24, { color: palette.textMuted, align: 'center' });
    label(dataset.readout.focusSite, f.x, f.y + 37, {
      color: palette.accent,
      align: 'center',
      tracking: 1.1,
      upper: true,
    });

    for (const reading of readings) {
      label(reading.text, reading.x, reading.y + 3, { color: reading.hue, align: 'center' });
    }
  }

  return {
    /** Resolved token strings for the current theme, plus the `light` flag. */
    setPalette(next) {
      palette = next;
    },

    /**
     * Fit the 1440x1160 world into the canvas box. The page uses the same
     * transform to map a pointer back into world space, so a gesture lands on
     * the mark the reader aimed at.
     */
    setViewport({ width, height, dpr }) {
      const scale = Math.min(width / dataset.world.width, height / dataset.world.height);
      view = {
        width,
        height,
        dpr,
        scale,
        offsetX: (width - dataset.world.width * scale) / 2,
        offsetY: (height - dataset.world.height * scale) / 2,
      };
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
      return view;
    },

    get viewport() {
      return { ...view };
    },

    /** World → canvas-CSS-pixel, for hit testing and for the QA harness. */
    toScreen(worldX, worldY) {
      return { x: view.offsetX + worldX * view.scale, y: view.offsetY + worldY * view.scale };
    },
    /** Canvas-CSS-pixel → world. */
    toWorld(screenX, screenY) {
      return { x: (screenX - view.offsetX) / view.scale, y: (screenY - view.offsetY) / view.scale };
    },

    /**
     * Paint one frame.
     *
     * @param {object} frame
     * @param {Float64Array} frame.positions  world coords, dataset node order
     * @param {Float64Array} frame.stretches  signed px off rest length, edge order
     * @param {Float64Array} frame.bloom      hover bloom per node, [0,1]
     * @param {string|null} frame.draggingId
     * @param {boolean} frame.reducedMotion
     */
    draw(frame) {
      if (!palette) throw new Error('setPalette must be called before draw');
      const { positions, stretches, bloom, draggingId = null, reducedMotion = false } = frame;
      ctx.setTransform(view.dpr, 0, 0, view.dpr, 0, 0);
      ctx.clearRect(0, 0, view.width, view.height);
      ctx.translate(view.offsetX, view.offsetY);
      ctx.scale(view.scale, view.scale);
      ctx.lineJoin = 'round';
      ctx.lineCap = 'round';

      drawRings();
      drawUnderlay(positions);
      drawMembranes(positions);
      const readings = drawChannels(positions, stretches, reducedMotion);
      drawPorts(positions);
      drawBloom(positions, bloom, palette.light);
      drawNodes(positions, palette.light, draggingId);
      drawFocus(positions);
      drawLabels(positions, readings);
    },
  };
}
