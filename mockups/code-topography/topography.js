/* Shared geometry for the code-topography mockups.
 *
 * Two things live here and nothing else: (1) the kind→hue arithmetic, copied
 * verbatim from dashboard/src/viz/graph/kindColor.ts so a `struct` is the same
 * hue on these pages as it is on the Code workspace's spine; (2) the relief
 * primitives — seeded blob outlines and bundled flow splines — that all four
 * pages draw with. Every function here is deterministic: the same fixture
 * produces the same picture on every screenshot run.
 */

/* Classic script, not an ES module: these pages are opened straight off
 * disk (file://), where module scripts are blocked by CORS. Everything hangs
 * off one global namespace instead. */
window.TOPO = (function () {
'use strict';

/* ---- kind hue: verbatim transcription of kindColor.ts ------------------- */

function hashKind(kind) {
  let hash = 0;
  for (let index = 0; index < kind.length; index += 1) {
    hash = (hash * 31 + kind.charCodeAt(index)) >>> 0;
  }
  return hash;
}

/** @param light whether the kind is drawn against a light medium. */
function kindColor(kind, light) {
  const hash = hashKind(kind);
  const chroma = (light ? 0.135 : 0.152) + ((hash >>> 9) % 6) * 0.012;
  const lightness = light ? 0.55 : 0.72;
  return `oklch(${lightness} ${chroma.toFixed(3)} ${186 + (hash % 148)})`;
}

/** Both sides of the hue as custom properties, so a mark carries dark+light
 * and the stylesheet picks — no theme observer, exactly as the app does it. */
function kindVars(kind) {
  return `--kind-dark:${kindColor(kind, false)};--kind-light:${kindColor(kind, true)}`;
}

/**
 * The paint every kind-tinted mark in these pages actually uses.
 *
 * A canvas has to be handed a resolved colour, but these sheets are SVG, so a
 * mark can carry both sides of the hue and let the stylesheet pick — which is
 * how the console answers a theme flip everywhere else (see `kindColorVars`).
 * The first request for a kind registers one custom property with its dark
 * value and a `[data-theme='light']` override with its light value, and every
 * call site just gets `var(--p-…)` back. No observer, no re-render, no second
 * copy of the arithmetic.
 */
let paletteSheet = null;
const registered = Object.create(null);
function paint(kind) {
  const key = 'p-' + String(kind).replace(/[^a-z0-9]+/gi, '-').toLowerCase();
  if (!registered[key]) {
    registered[key] = true;
    if (!paletteSheet) {
      const node = document.createElement('style');
      document.head.appendChild(node);
      paletteSheet = node.sheet;
    }
    paletteSheet.insertRule(
      ':root{--' + key + ':' + kindColor(kind, false) + '}',
      paletteSheet.cssRules.length,
    );
    paletteSheet.insertRule(
      ":root[data-theme='light']{--" + key + ':' + kindColor(kind, true) + '}',
      paletteSheet.cssRules.length,
    );
  }
  return 'var(--' + key + ')';
}

/* ---- deterministic noise ------------------------------------------------ */

function rng(seed) {
  let a = seed >>> 0;
  return function next() {
    a += 0x6d2b79f5;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * A closed relief outline. Not decorative noise: `rough` is handed the
 * region's own irregularity measure by the caller, so a lumpy coastline means
 * a lumpy module and a smooth one means a tight one.
 */
function blobPath(cx, cy, radiusX, radiusY, seed, rough = 0.16, samples = 84) {
  const random = rng(seed);
  const harmonics = [
    { k: 2 + Math.floor(random() * 2), a: rough * (0.55 + random() * 0.5), p: random() * 6.283 },
    { k: 3 + Math.floor(random() * 3), a: rough * (0.34 + random() * 0.4), p: random() * 6.283 },
    { k: 6 + Math.floor(random() * 4), a: rough * (0.14 + random() * 0.2), p: random() * 6.283 },
  ];
  let d = '';
  for (let i = 0; i <= samples; i += 1) {
    const t = (i / samples) * Math.PI * 2;
    let m = 1;
    for (const h of harmonics) m += h.a * Math.sin(h.k * t + h.p);
    const x = cx + Math.cos(t) * radiusX * m;
    const y = cy + Math.sin(t) * radiusY * m;
    d += (i === 0 ? 'M' : 'L') + x.toFixed(2) + ' ' + y.toFixed(2);
  }
  return d + 'Z';
}

/**
 * A hierarchically bundled flow. Waypoints are the bundle's trunk; the spline
 * is a Catmull–Rom through them converted to cubic beziers, which is what
 * makes several flows sharing a trunk read as one braided channel rather than
 * as parallel arrows.
 */
function bundledPath(points, tension = 0.5) {
  if (points.length < 2) return '';
  const p = [points[0], ...points, points[points.length - 1]];
  let d = `M${p[1][0].toFixed(2)} ${p[1][1].toFixed(2)}`;
  for (let i = 1; i < p.length - 2; i += 1) {
    const [x0, y0] = p[i - 1];
    const [x1, y1] = p[i];
    const [x2, y2] = p[i + 1];
    const [x3, y3] = p[i + 2];
    const c1x = x1 + ((x2 - x0) / 6) * tension * 2;
    const c1y = y1 + ((y2 - y0) / 6) * tension * 2;
    const c2x = x2 - ((x3 - x1) / 6) * tension * 2;
    const c2y = y2 - ((y3 - y1) / 6) * tension * 2;
    d += `C${c1x.toFixed(2)} ${c1y.toFixed(2)},${c2x.toFixed(2)} ${c2y.toFixed(2)},${x2.toFixed(2)} ${y2.toFixed(2)}`;
  }
  return d;
}

/**
 * A tapered ribbon along a spline: width is a MEASURED quantity at each end
 * (call sites entering, call sites leaving), so a channel that loses volume
 * to a branch visibly narrows past the fork.
 */
function ribbonPath(points, widthStart, widthEnd) {
  const n = points.length;
  if (n < 2) return '';
  const normals = points.map((pt, i) => {
    const a = points[Math.max(0, i - 1)];
    const b = points[Math.min(n - 1, i + 1)];
    const dx = b[0] - a[0];
    const dy = b[1] - a[1];
    const len = Math.hypot(dx, dy) || 1;
    return [-dy / len, dx / len];
  });
  const w = (i) => (widthStart + (widthEnd - widthStart) * (i / (n - 1))) / 2;
  const upper = points.map((pt, i) => [pt[0] + normals[i][0] * w(i), pt[1] + normals[i][1] * w(i)]);
  const lower = points
    .map((pt, i) => [pt[0] - normals[i][0] * w(i), pt[1] - normals[i][1] * w(i)])
    .reverse();
  // The return leg starts at the far end of the upper edge, so its leading
  // moveto becomes a lineto — concatenating the two `M` paths without doing
  // that fuses two coordinates into one enormous number and the ribbon flies
  // off the canvas.
  return bundledPath(upper) + 'L' + bundledPath(lower).slice(1) + 'Z';
}

/* ---- small DOM helpers -------------------------------------------------- */

const NS = 'http://www.w3.org/2000/svg';

function el(name, attrs = {}, parent = null) {
  const node = document.createElementNS(NS, name);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === null || v === undefined) continue;
    node.setAttribute(k, String(v));
  }
  if (parent) parent.appendChild(node);
  return node;
}

function text(parent, x, y, content, cls = 'lbl', extra = {}) {
  const node = el('text', { x, y, class: cls, ...extra }, parent);
  node.textContent = content;
  return node;
}

  return { hashKind, kindColor, kindVars, paint, rng, blobPath, bundledPath, ribbonPath, NS, el, text };
})();
