/**
 * TRUST FIELD: every fact placed on two measured axes.
 *
 *   x   trust, one linear scale, 0 at the left edge of the plot
 *   y   the loaded row's `updated_at`, newest at the top
 *   r   retrieval count, area-proportional to the field's largest
 *
 * A fact whose trust the store did not report sits in a gutter right of the
 * scale, and a fact whose row carried no `updated_at` (or whose row this read
 * did not load) sits in a gutter under the plot; both gutters print their
 * counts. Entities are hairline envelopes around the facts that cite them by
 * name. Zooming into an entity re-derives both domains from its members, so
 * the axes stay printed in true units and text stays at screen size.
 *
 * Pure: the same scene, box and zoom yield the same layout.
 */
import type { FactScene, SceneFact, SceneRelation } from './factScene.ts';

const MARGIN = { left: 52, right: 58, top: 14, bottom: 42 };
const GUTTER = 22;
const R_MIN = 3;
const R_SPAN = 6;
const LABEL_BUDGET = 6;
const ZOOM_LABEL_BUDGET = 14;
const LABEL_CHARS = 30;
const LABEL_PX = 11;
const LABEL_ADVANCE = LABEL_PX * 0.6;

export interface FieldPoint {
  readonly fact: SceneFact;
  readonly x: number;
  readonly y: number;
  readonly r: number;
  readonly labelled: boolean;
  /** Inside the plot or one of its gutters under the current domains. */
  readonly visible: boolean;
  /** A member of the zoomed entity (every fact when not zoomed), and visible. */
  readonly inZoom: boolean;
}

export interface FieldEnvelope {
  readonly entityId: string;
  readonly label: string;
  readonly count: number;
  readonly d: string;
  readonly labelX: number;
  readonly labelY: number;
}

export interface FieldRelation {
  readonly relation: SceneRelation;
  readonly x1: number;
  readonly y1: number;
  readonly x2: number;
  readonly y2: number;
  readonly loud: boolean;
}

export interface FieldTick {
  readonly at: number;
  readonly label: string;
}

export interface TrustFieldLayout {
  readonly width: number;
  readonly height: number;
  readonly plot: { readonly x0: number; readonly x1: number; readonly y0: number; readonly y1: number };
  readonly trustDomain: readonly [number, number];
  /** Microseconds, oldest then newest; `null` when no loaded row carried one. */
  readonly timeDomain: readonly [number, number] | null;
  readonly xTicks: readonly FieldTick[];
  readonly yTicks: readonly FieldTick[];
  readonly points: readonly FieldPoint[];
  readonly envelopes: readonly FieldEnvelope[];
  readonly relations: readonly FieldRelation[];
  readonly trustAbsent: number;
  readonly timeAbsent: number;
  readonly retrievalCeiling: number;
  readonly zoom: { readonly entityId: string; readonly label: string; readonly count: number } | null;
}

export function layoutTrustField(
  scene: FactScene,
  box: { width: number; height: number },
  zoomEntityId: string | null,
): TrustFieldLayout {
  const plot = {
    x0: MARGIN.left,
    x1: Math.max(MARGIN.left + 80, box.width - MARGIN.right),
    y0: MARGIN.top,
    y1: Math.max(MARGIN.top + 60, box.height - MARGIN.bottom),
  };
  const zoomEntity = zoomEntityId ? scene.entities.find((entity) => entity.nodeId === zoomEntityId) : undefined;
  const members = zoomEntity ? new Set(zoomEntity.factIds) : null;
  const domainFacts = members ? scene.facts.filter((fact) => members.has(fact.nodeId)) : scene.facts;

  const trusts = domainFacts.flatMap((fact) => (fact.trust == null ? [] : [clamp01(fact.trust)]));
  const trustDomain: [number, number] =
    members && trusts.length > 0 ? paddedDomain(Math.min(...trusts), Math.max(...trusts), 0.1, [0, 1]) : [0, 1];
  const times = domainFacts.flatMap((fact) => (fact.updatedAt == null ? [] : [fact.updatedAt]));
  const timeDomain: [number, number] | null =
    times.length === 0 ? null : paddedTime(Math.min(...times), Math.max(...times));

  const retrievalCeiling = scene.facts.reduce((max, fact) => Math.max(max, fact.retrievals ?? 0), 0);
  const xOf = (trust: number) =>
    plot.x0 + ((trust - trustDomain[0]) / (trustDomain[1] - trustDomain[0])) * (plot.x1 - plot.x0);
  const yOf = (at: number) =>
    timeDomain == null
      ? (plot.y0 + plot.y1) / 2
      : plot.y0 + ((timeDomain[1] - at) / (timeDomain[1] - timeDomain[0])) * (plot.y1 - plot.y0);

  let trustAbsent = 0;
  let timeAbsent = 0;
  const trustGutterX = plot.x1 + GUTTER;
  const timeGutterY = plot.y1 + GUTTER - 6;
  const placed = scene.facts.map((fact) => {
    const trust = fact.trust == null ? null : clamp01(fact.trust);
    if (trust == null) trustAbsent += 1;
    if (fact.updatedAt == null) timeAbsent += 1;
    const x = trust == null ? trustGutterX : xOf(trust);
    const y = fact.updatedAt == null ? timeGutterY : yOf(fact.updatedAt);
    const r =
      fact.retrievals == null || retrievalCeiling === 0
        ? R_MIN
        : R_MIN + R_SPAN * Math.sqrt(fact.retrievals / retrievalCeiling);
    const inZoom = members ? members.has(fact.nodeId) : true;
    const visible = x >= plot.x0 - 1 && x <= trustGutterX + 1 && y >= plot.y0 - 1 && y <= timeGutterY + 1;
    return { fact, x: round(x), y: round(y), r: round(r), visible, inZoom: inZoom && visible };
  });

  // Labels: the most retrieved (then most trusted) facts, or every member of
  // the zoomed entity, greedily refusing any whose text box would overprint.
  const budget = members ? ZOOM_LABEL_BUDGET : LABEL_BUDGET;
  const candidates = placed
    .filter((point) => point.inZoom && !point.fact.restricted)
    .sort(
      (a, b) =>
        (b.fact.retrievals ?? -1) - (a.fact.retrievals ?? -1) ||
        (b.fact.trust ?? -1) - (a.fact.trust ?? -1) ||
        a.fact.factId.localeCompare(b.fact.factId),
    );
  const boxes: { x0: number; x1: number; y0: number; y1: number }[] = [];
  const labelled = new Set<string>();
  for (const point of candidates) {
    if (labelled.size >= budget) break;
    const w = Math.min(point.fact.label.length, LABEL_CHARS) * LABEL_ADVANCE;
    const right = point.x + point.r + 4 + w <= plot.x1;
    const x0 = right ? point.x + point.r + 4 : point.x - point.r - 4 - w;
    const candidate = { x0, x1: x0 + w, y0: point.y - 8, y1: point.y + 5 };
    if (boxes.some((other) => overlaps(other, candidate))) continue;
    boxes.push(candidate);
    labelled.add(point.fact.nodeId);
  }
  const points: FieldPoint[] = placed.map((point) => ({ ...point, labelled: labelled.has(point.fact.nodeId) }));
  const at = new Map(points.map((point) => [point.fact.nodeId, point]));

  const envelopes: FieldEnvelope[] = [];
  for (const entity of scene.entities) {
    if (members && entity.nodeId !== zoomEntityId) continue;
    const pts = entity.factIds.flatMap((id) => {
      const point = at.get(id);
      return point && point.inZoom ? [point] : [];
    });
    if (pts.length === 0) continue;
    const hull = convexHull(pts.flatMap((point) => ring(point.x, point.y, point.r + 7)));
    const top = hull.reduce((best, p) => (p.y < best.y ? p : best), hull[0]!);
    envelopes.push({
      entityId: entity.nodeId,
      label: entity.label,
      count: entity.factIds.length,
      d: `M ${hull.map((p) => `${round(p.x)} ${round(p.y)}`).join(' L ')} Z`,
      labelX: round(top.x),
      labelY: round(top.y - 4),
    });
  }

  const relations: FieldRelation[] = [];
  for (const relation of scene.relations) {
    const a = at.get(relation.source);
    const b = at.get(relation.target);
    if (!a || !b) continue;
    if (members && !(a.inZoom || b.inZoom)) continue;
    relations.push({
      relation,
      x1: a.x,
      y1: a.y,
      x2: b.x,
      y2: b.y,
      loud: relation.kind === 'contradicts' || relation.kind === 'supersedes',
    });
  }

  return {
    width: box.width,
    height: box.height,
    plot,
    trustDomain,
    timeDomain,
    xTicks: trustTicks(trustDomain).map((value) => ({ at: round(xOf(value)), label: value.toFixed(2) })),
    yTicks: timeDomain ? timeTicks(timeDomain).map((value) => ({ at: round(yOf(value)), label: shortDate(value) })) : [],
    points,
    envelopes,
    relations,
    trustAbsent,
    timeAbsent,
    retrievalCeiling,
    zoom: zoomEntity ? { entityId: zoomEntity.nodeId, label: zoomEntity.label, count: zoomEntity.factIds.length } : null,
  };
}

function clamp01(value: number): number {
  return Math.max(0, Math.min(1, value));
}

function paddedDomain(lo: number, hi: number, minSpan: number, bounds: [number, number]): [number, number] {
  const span = Math.max(minSpan, hi - lo);
  const mid = (lo + hi) / 2;
  let a = mid - span * 0.6;
  let b = mid + span * 0.6;
  if (a < bounds[0]) [a, b] = [bounds[0], bounds[0] + (b - a)];
  if (b > bounds[1]) [a, b] = [Math.max(bounds[0], bounds[1] - (b - a)), bounds[1]];
  return [round(a), round(b)];
}

const HOUR_MICROS = 3_600_000_000;
const DAY_MICROS = 24 * HOUR_MICROS;

/** At least a day wide, centred on the data, so one row still has an axis. */
function paddedTime(oldest: number, newest: number): [number, number] {
  const span = Math.max(DAY_MICROS, newest - oldest);
  const mid = (oldest + newest) / 2;
  const half = span / 2 + span * 0.06;
  return [mid - half, mid + half];
}

function trustTicks([lo, hi]: readonly [number, number]): number[] {
  const span = hi - lo;
  const step = span > 0.5 ? 0.2 : span > 0.2 ? 0.1 : 0.05;
  const out: number[] = [];
  for (let value = Math.ceil(lo / step - 1e-9) * step; value <= hi + 1e-9; value += step) {
    out.push(Math.round(value * 100) / 100);
  }
  return out;
}

function timeTicks([oldest, newest]: readonly [number, number]): number[] {
  const days = (newest - oldest) / DAY_MICROS;
  const stepDays = days > 60 ? 14 : days > 28 ? 7 : days > 10 ? 3 : 1;
  const step = stepDays * DAY_MICROS;
  const out: number[] = [];
  for (let value = Math.ceil(oldest / step) * step; value <= newest; value += step) out.push(value);
  return out;
}

export function shortDate(micros: number): string {
  return new Date(micros / 1000).toLocaleDateString('en-US', { month: 'short', day: 'numeric', timeZone: 'UTC' });
}

function ring(x: number, y: number, r: number): { x: number; y: number }[] {
  return Array.from({ length: 10 }, (_, index) => {
    const angle = (index / 10) * Math.PI * 2;
    return { x: x + Math.cos(angle) * r, y: y + Math.sin(angle) * r };
  });
}

/** Andrew's monotone chain. */
function convexHull(points: { x: number; y: number }[]): { x: number; y: number }[] {
  const sorted = [...points].sort((a, b) => a.x - b.x || a.y - b.y);
  const cross = (o: { x: number; y: number }, a: { x: number; y: number }, b: { x: number; y: number }) =>
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
  const lower: { x: number; y: number }[] = [];
  for (const p of sorted) {
    while (lower.length >= 2 && cross(lower[lower.length - 2]!, lower[lower.length - 1]!, p) <= 0) lower.pop();
    lower.push(p);
  }
  const upper: { x: number; y: number }[] = [];
  for (const p of [...sorted].reverse()) {
    while (upper.length >= 2 && cross(upper[upper.length - 2]!, upper[upper.length - 1]!, p) <= 0) upper.pop();
    upper.push(p);
  }
  return [...lower.slice(0, -1), ...upper.slice(0, -1)];
}

function overlaps(a: { x0: number; x1: number; y0: number; y1: number }, b: { x0: number; x1: number; y0: number; y1: number }) {
  return a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1;
}

function round(value: number): number {
  return Math.round(value * 100) / 100;
}
