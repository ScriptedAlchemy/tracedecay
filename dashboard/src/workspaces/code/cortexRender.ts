/**
 * Canvas2D renderer for the CORTEX relief sheet (plan 11b `:191` — "Canvas2D
 * first: terrain contours, membranes and bundled flows are path-heavy 2D work
 * where Canvas2D plus offscreen layering is simpler and theme-safe").
 *
 * This module draws and decides nothing. It is handed a `CortexModel` whose
 * every position, radius and ring count was computed in `cortexRelief.ts` from
 * one wire reading, plus a resolved palette, and it paints one frame. It never
 * reads a payload, never writes a caption, and never invents a mark: a region
 * with no relief is drawn hollow because the MODEL says its contour count is
 * zero, not because this file decided a hollow shape looked better there.
 *
 * Canvas2D cannot read CSS custom properties, so the composing component
 * samples the token block once at mount and once per theme flip and hands the
 * resolved strings in — the same arrangement `viz/trace/palette.ts` uses, which
 * keeps `tokens.css` the single source of the instrument's colour without a
 * `getComputedStyle` call inside a draw.
 */
import { kindColor } from '../../viz/graph/kindColor.ts';
import {
  CONTOUR_INDEX_EVERY,
  MAX_DRAWN_CONTOURS,
  RELIEF_ASPECT,
  type CortexModel,
  type CortexRegion,
} from './cortexRelief.ts';

export interface CortexPalette {
  readonly surface0: string;
  readonly surface1: string;
  readonly textPrimary: string;
  readonly textMuted: string;
  readonly edgeSubtle: string;
  readonly edgeStrong: string;
  readonly grid: string;
  readonly accent: string;
  readonly stateUnknown: string;
  readonly light: boolean;
}

const FALLBACK: Record<string, string> = {
  '--raw-surface-0': '#141619',
  '--raw-surface-1': '#1c2029',
  '--raw-text-primary': '#eef0f4',
  '--raw-text-muted': '#9aa1b0',
  '--raw-edge-subtle': '#333a46',
  '--raw-edge-strong': '#4c5464',
  '--raw-grid': '#2a2f38',
  '--raw-accent': '#5fd0e0',
  '--raw-state-unknown': '#8d919b',
};

export function resolveCortexPalette(element: HTMLElement): CortexPalette {
  const style = getComputedStyle(element);
  const token = (name: string): string =>
    style.getPropertyValue(name).trim() || FALLBACK[name] || '#888888';
  return {
    surface0: token('--raw-surface-0'),
    surface1: token('--raw-surface-1'),
    textPrimary: token('--raw-text-primary'),
    textMuted: token('--raw-text-muted'),
    edgeSubtle: token('--raw-edge-subtle'),
    edgeStrong: token('--raw-edge-strong'),
    grid: token('--raw-grid'),
    accent: token('--raw-accent'),
    stateUnknown: token('--raw-state-unknown'),
    light: document.documentElement.dataset['theme'] === 'light',
  };
}

/* ---- shape -------------------------------------------------------------- */

const OUTLINE_POINTS = 30;

function hashString(value: string): number {
  let hash = 0;
  for (let index = 0; index < value.length; index += 1) {
    hash = (hash * 31 + value.charCodeAt(index)) >>> 0;
  }
  return hash;
}

/**
 * A landform outline: an ellipse with three fixed harmonics whose phases come
 * from a hash of the directory name.
 *
 * The wobble is deterministic (no `Math.random`, so a screenshot is stable) and
 * it carries NO measurement — the legend says so out loud. Its only job is to
 * stop nineteen ellipses from reading as nineteen buttons, which is the exact
 * "nothing here is a box" instruction the sheet was drawn under. The harmonics
 * are mean-preserving in radius, so two regions with the same file count still
 * enclose the same area.
 */
export function reliefOutline(
  directory: string,
  radius: number,
  points: number = OUTLINE_POINTS,
): readonly { readonly x: number; readonly y: number }[] {
  const seed = hashString(directory);
  const phase = (shift: number): number => ((seed >>> shift) % 628) / 100;
  const out: { x: number; y: number }[] = [];
  for (let index = 0; index < points; index += 1) {
    const t = (index / points) * Math.PI * 2;
    const wobble =
      1 +
      0.085 * Math.sin(3 * t + phase(0)) +
      0.055 * Math.sin(5 * t + phase(7)) +
      0.03 * Math.sin(7 * t + phase(13));
    out.push({
      x: Math.cos(t) * radius * wobble * RELIEF_ASPECT.x,
      y: Math.sin(t) * radius * wobble * RELIEF_ASPECT.y,
    });
  }
  return out;
}

/** Half-axes of the hit ellipse for a region, in world units. */
export function reliefExtent(radius: number): { readonly rx: number; readonly ry: number } {
  return { rx: radius * RELIEF_ASPECT.x, ry: radius * RELIEF_ASPECT.y };
}

/** Which region a world point lands in, innermost (smallest) first so a small
 * body sitting over a large one is still reachable. */
export function hitRegion(model: CortexModel, x: number, y: number): string | null {
  const candidates = [...model.drawnRegions].sort(
    (a, b) => (a.radius ?? 0) - (b.radius ?? 0),
  );
  for (const region of candidates) {
    if (region.x === null || region.y === null || region.radius === null) continue;
    const { rx, ry } = reliefExtent(region.radius);
    const dx = (x - region.x) / rx;
    const dy = (y - region.y) / ry;
    if (dx * dx + dy * dy <= 1) return region.directory;
  }
  return null;
}

/* ---- the renderer ------------------------------------------------------- */

export interface CortexViewport {
  readonly width: number;
  readonly height: number;
  readonly dpr: number;
}

export interface CortexFrame {
  /** Directory the reader has selected, or null. */
  readonly selected: string | null;
  /** Directory of the region containing the workspace's current symbol focus.
   * This is the LENS continuum's "you are here" and nothing else. */
  readonly focused: string | null;
}

export interface CortexRenderer {
  setPalette(palette: CortexPalette): void;
  setViewport(viewport: CortexViewport): void;
  draw(frame: CortexFrame): void;
  toWorld(x: number, y: number): { x: number; y: number };
}

export function createCortexRenderer(
  canvas: HTMLCanvasElement,
  model: CortexModel,
): CortexRenderer {
  const ctx = canvas.getContext('2d');
  if (!ctx) throw new Error('no 2d context');

  let palette: CortexPalette = {
    surface0: FALLBACK['--raw-surface-0']!,
    surface1: FALLBACK['--raw-surface-1']!,
    textPrimary: FALLBACK['--raw-text-primary']!,
    textMuted: FALLBACK['--raw-text-muted']!,
    edgeSubtle: FALLBACK['--raw-edge-subtle']!,
    edgeStrong: FALLBACK['--raw-edge-strong']!,
    grid: FALLBACK['--raw-grid']!,
    accent: FALLBACK['--raw-accent']!,
    stateUnknown: FALLBACK['--raw-state-unknown']!,
    light: false,
  };
  let viewport: CortexViewport = { width: model.world.width, height: model.world.height, dpr: 1 };
  let scale = 1;

  const mono = (size: number, weight = 400): string =>
    `${weight} ${size}px ui-monospace, "IBM Plex Mono", SFMono-Regular, monospace`;

  function tracePath(
    points: readonly { readonly x: number; readonly y: number }[],
    cx: number,
    cy: number,
    shrink: number,
  ): void {
    ctx!.beginPath();
    points.forEach((point, index) => {
      const x = cx + point.x * shrink;
      const y = cy + point.y * shrink;
      if (index === 0) ctx!.moveTo(x, y);
      else ctx!.lineTo(x, y);
    });
    ctx!.closePath();
  }

  function drawStrata(): void {
    ctx!.save();
    ctx!.lineWidth = 1 / scale;
    ctx!.font = mono(13);
    ctx!.textBaseline = 'middle';
    for (const band of model.strata) {
      ctx!.strokeStyle = palette.grid;
      ctx!.setLineDash([3 / scale, 5 / scale]);
      ctx!.beginPath();
      ctx!.moveTo(96, band.y);
      ctx!.lineTo(model.world.width - 24, band.y);
      ctx!.stroke();
      ctx!.setLineDash([]);
      ctx!.fillStyle = palette.textMuted;
      ctx!.textAlign = 'left';
      ctx!.fillText(`depth ${band.depth}`, 12, band.y - 8);
      ctx!.fillStyle = palette.edgeStrong;
      ctx!.fillText(
        band.depth === 0 ? 'BEDROCK' : band.depth === model.maxDepth ? 'RIDGE' : '',
        12,
        band.y + 8,
      );
      // The plan's own second-order note: the strata are drawn evenly spaced
      // while their population is not. Printing the population on the axis is
      // how a reader sees that without an area-preserving scale.
      ctx!.fillStyle = palette.textMuted;
      ctx!.textAlign = 'right';
      ctx!.fillText(`${band.regions}`, 92, band.y + 8);
    }
    ctx!.restore();
  }

  function drawRegion(region: CortexRegion, frame: CortexFrame): void {
    if (region.x === null || region.y === null || region.radius === null) return;
    const outline = reliefOutline(region.directory, region.radius);
    const selected = frame.selected === region.directory;
    const focused = frame.focused === region.directory;
    const hue = kindColor(region.directory, palette.light);

    ctx!.save();
    if (region.contours === 0) {
      // Measured zero internal edges. Drawn at true position and true area,
      // dashed and empty — absence as a mark, never as flat ground.
      tracePath(outline, region.x, region.y, 1);
      ctx!.setLineDash([5 / scale, 4 / scale]);
      ctx!.strokeStyle = palette.stateUnknown;
      ctx!.lineWidth = 1.2 / scale;
      ctx!.stroke();
      ctx!.setLineDash([]);
    } else {
      tracePath(outline, region.x, region.y, 1);
      ctx!.fillStyle = hue;
      ctx!.globalAlpha = palette.light ? 0.16 : 0.2;
      ctx!.fill();
      ctx!.globalAlpha = 1;
      ctx!.strokeStyle = hue;
      ctx!.lineWidth = 1.1 / scale;
      ctx!.stroke();

      const rings = Math.min(region.contours, MAX_DRAWN_CONTOURS);
      for (let ring = 1; ring <= rings; ring += 1) {
        const shrink = 1 - (ring / (rings + 1)) * 0.86;
        tracePath(outline, region.x, region.y, shrink);
        const indexed = ring % CONTOUR_INDEX_EVERY === 0;
        ctx!.strokeStyle = hue;
        ctx!.globalAlpha = indexed ? 0.95 : 0.5;
        ctx!.lineWidth = (indexed ? 1.4 : 0.7) / scale;
        ctx!.stroke();
      }
      ctx!.globalAlpha = 1;
    }

    if (selected || focused) {
      tracePath(outline, region.x, region.y, 1.12);
      ctx!.strokeStyle = palette.accent;
      ctx!.lineWidth = (selected ? 2 : 1.2) / scale;
      if (focused && !selected) ctx!.setLineDash([6 / scale, 4 / scale]);
      ctx!.stroke();
      ctx!.setLineDash([]);
    }
    ctx!.restore();

    const { ry } = reliefExtent(region.radius);
    ctx!.save();
    ctx!.textAlign = 'center';
    ctx!.textBaseline = 'alphabetic';
    ctx!.fillStyle = palette.textPrimary;
    ctx!.font = mono(14, 500);
    ctx!.fillText(region.label, region.x, region.y + 4);
    ctx!.fillStyle = palette.textMuted;
    ctx!.font = mono(11);
    ctx!.fillText(`${region.fileCount} files`, region.x, region.y + 18);
    ctx!.fillText(
      region.contours === 0 ? 'no relief' : `${region.density.toFixed(2)} e/f`,
      region.x,
      region.y + 30,
    );
    ctx!.font = mono(10);
    ctx!.fillStyle = palette.edgeStrong;
    ctx!.fillText(region.directory, region.x, region.y - ry - 6);
    ctx!.restore();
  }

  return {
    setPalette(next) {
      palette = next;
    },
    setViewport(next) {
      viewport = next;
      canvas.width = Math.max(1, Math.round(next.width * next.dpr));
      canvas.height = Math.max(1, Math.round(next.height * next.dpr));
      canvas.style.width = `${next.width}px`;
      canvas.style.height = `${next.height}px`;
      scale = next.width / model.world.width;
    },
    draw(frame) {
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      ctx.fillStyle = palette.surface0;
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      ctx.setTransform(
        viewport.dpr * scale,
        0,
        0,
        viewport.dpr * scale,
        0,
        0,
      );
      drawStrata();
      // Largest first, so a small body never disappears under a massif.
      const ordered = [...model.drawnRegions].sort(
        (a, b) => (b.radius ?? 0) - (a.radius ?? 0),
      );
      for (const region of ordered) drawRegion(region, frame);
    },
    toWorld(x, y) {
      return { x: x / scale, y: y / scale };
    },
  };
}
