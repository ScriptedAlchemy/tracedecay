/**
 * CONSTELLATION LATTICE: entities as hubs settled by a force layout, facts as
 * their satellites.
 *
 * Hubs are the entities the drawn facts cite by name, plus one `no entity`
 * hub for facts that cite none. Two hubs attract in proportion to the
 * fact-to-fact relations between their satellites; ForceAtlas2 runs a fixed
 * number of iterations from positions derived from sorted identity, so the
 * same scene settles to the same picture. Settled coordinates are fitted to
 * the measured box and separated so no two orbits overlap.
 *
 * A fact orbits the first hub it cites (sorted by label); any other hub it
 * cites gets a hairline spoke. An orbit holds as many satellites as its
 * circumference fits; the rest are one aggregate mark printing their exact
 * count and trust range. Cross-hub relations are bundled per hub pair and
 * relation kind, one curve each with its exact count.
 *
 * Pure: the same scene and box yield the same layout.
 */
import Graph from 'graphology';
import forceAtlas2 from 'graphology-layout-forceatlas2';

import type { FactScene, SceneFact, SceneRelation } from './factScene.ts';

export const NO_ENTITY_HUB = 'hub:no-entity';
const SATELLITE_SPACING = 16;
const ITERATIONS = 160;
const MARGIN = 58;
const KIND_ORDER: readonly SceneRelation['kind'][] = [
  'supports',
  'derived_from',
  'supersedes',
  'contradicts',
  'active_assertion',
  'evidence_anchor',
];

export interface LatticeHub {
  readonly id: string;
  readonly label: string;
  readonly x: number;
  readonly y: number;
  readonly r: number;
  readonly orbit: number;
  /** Facts orbiting this hub. */
  readonly count: number;
}

export interface LatticeSatellite {
  readonly fact: SceneFact;
  readonly hubId: string;
  readonly x: number;
  readonly y: number;
  readonly r: number;
  /** No helpful feedback recorded: drawn as a ring rather than a disc. */
  readonly unconfirmed: boolean;
  readonly labelAnchor: 'start' | 'end';
}

export interface LatticeAggregate {
  readonly hubId: string;
  readonly x: number;
  readonly y: number;
  readonly count: number;
  readonly trustRange: readonly [number, number] | null;
}

export interface LatticeBundle {
  readonly key: string;
  readonly kind: SceneRelation['kind'];
  readonly from: string;
  readonly to: string;
  readonly count: number;
  readonly relations: readonly SceneRelation[];
  readonly d: string;
  readonly midX: number;
  readonly midY: number;
  /** Hub-to-hub distance; a short bundle prints its label only on focus. */
  readonly length: number;
}

export interface LatticeChord {
  readonly relation: SceneRelation;
  readonly d: string;
}

export interface LatticeSpoke {
  readonly factNodeId: string;
  readonly hubId: string;
  readonly x1: number;
  readonly y1: number;
  readonly x2: number;
  readonly y2: number;
  readonly primary: boolean;
}

export interface LatticeLayout {
  readonly width: number;
  readonly height: number;
  readonly hubs: readonly LatticeHub[];
  readonly satellites: readonly LatticeSatellite[];
  readonly aggregates: readonly LatticeAggregate[];
  readonly bundles: readonly LatticeBundle[];
  readonly chords: readonly LatticeChord[];
  readonly spokes: readonly LatticeSpoke[];
  /** Relations with an end folded into an aggregate, counted. */
  readonly relationsIntoAggregates: number;
}

export function layoutLattice(scene: FactScene, box: { width: number; height: number }): LatticeLayout {
  const entityLabel = new Map(scene.entities.map((entity) => [entity.nodeId, entity.label]));
  const primaryOf = new Map<string, string>();
  const orbiting = new Map<string, SceneFact[]>();
  for (const fact of scene.facts) {
    const cited = [...fact.entityIds].sort((a, b) =>
      (entityLabel.get(a) ?? a).localeCompare(entityLabel.get(b) ?? b),
    );
    const hub = cited[0] ?? NO_ENTITY_HUB;
    primaryOf.set(fact.nodeId, hub);
    const list = orbiting.get(hub) ?? [];
    list.push(fact);
    orbiting.set(hub, list);
  }
  const hubIds = [
    ...scene.entities.map((entity) => entity.nodeId),
    ...(orbiting.has(NO_ENTITY_HUB) ? [NO_ENTITY_HUB] : []),
  ];

  // Settle hub positions.
  const graph = new Graph({ type: 'undirected' });
  hubIds.forEach((id, index) => {
    const angle = (index / Math.max(1, hubIds.length)) * Math.PI * 2;
    graph.addNode(id, { x: Math.cos(angle) * 100, y: Math.sin(angle) * 100, size: 1 + (orbiting.get(id)?.length ?? 0) });
  });
  for (const relation of scene.relations) {
    const a = primaryOf.get(relation.source);
    const b = primaryOf.get(relation.target);
    if (!a || !b || a === b) continue;
    if (graph.hasEdge(a, b)) graph.updateEdgeAttribute(a, b, 'weight', (weight: number) => weight + 1);
    else graph.addEdge(a, b, { weight: 1 });
  }
  if (graph.order > 1) {
    forceAtlas2.assign(graph, {
      iterations: ITERATIONS,
      settings: { ...forceAtlas2.inferSettings(graph), gravity: 1.2, scalingRatio: 12, barnesHutOptimize: false, adjustSizes: false },
    });
  }

  const sized = hubIds.map((id) => {
    const count = orbiting.get(id)?.length ?? 0;
    const r = 4 + 2.4 * Math.sqrt(count);
    const orbit = r + 16 + Math.min(count, 12) * 1.6;
    return { id, count, r, orbit, x: graph.getNodeAttribute(id, 'x') as number, y: graph.getNodeAttribute(id, 'y') as number };
  });
  fit(sized, box);
  separate(sized, box);

  const hubs: LatticeHub[] = sized.map((hub) => ({
    id: hub.id,
    label: hub.id === NO_ENTITY_HUB ? 'no entity cited' : (entityLabel.get(hub.id) ?? hub.id),
    x: round(hub.x),
    y: round(hub.y),
    r: round(hub.r),
    orbit: round(hub.orbit),
    count: hub.count,
  }));
  const hubAt = new Map(hubs.map((hub) => [hub.id, hub]));

  const satellites: LatticeSatellite[] = [];
  const aggregates: LatticeAggregate[] = [];
  const placedAt = new Map<string, { x: number; y: number }>();
  for (const hub of hubs) {
    const facts = [...(orbiting.get(hub.id) ?? [])].sort(
      (a, b) => (b.trust ?? -1) - (a.trust ?? -1) || a.factId.localeCompare(b.factId),
    );
    const capacity = Math.max(1, Math.floor((2 * Math.PI * hub.orbit) / SATELLITE_SPACING));
    const shown = facts.length > capacity ? facts.slice(0, capacity - 1) : facts;
    const folded = facts.slice(shown.length);
    const slots = shown.length + (folded.length > 0 ? 1 : 0);
    shown.forEach((fact, index) => {
      const angle = -Math.PI / 2 + (index / slots) * Math.PI * 2;
      const x = round(hub.x + Math.cos(angle) * hub.orbit);
      const y = round(hub.y + Math.sin(angle) * hub.orbit);
      placedAt.set(fact.nodeId, { x, y });
      satellites.push({
        fact,
        hubId: hub.id,
        x,
        y,
        r: round(3.4 + 2.6 * (fact.trust ?? 0)),
        unconfirmed: !fact.restricted && fact.trust != null && (fact.helpful ?? 0) === 0,
        labelAnchor: Math.cos(angle) >= 0 ? 'start' : 'end',
      });
    });
    if (folded.length > 0) {
      const angle = -Math.PI / 2 + (shown.length / slots) * Math.PI * 2;
      const trusts = folded.flatMap((fact) => (fact.trust == null ? [] : [fact.trust]));
      aggregates.push({
        hubId: hub.id,
        x: round(hub.x + Math.cos(angle) * hub.orbit),
        y: round(hub.y + Math.sin(angle) * hub.orbit),
        count: folded.length,
        trustRange: trusts.length ? [Math.min(...trusts), Math.max(...trusts)] : null,
      });
    }
  }

  const spokes: LatticeSpoke[] = [];
  for (const satellite of satellites) {
    for (const entityId of satellite.fact.entityIds) {
      const hub = hubAt.get(entityId);
      if (!hub) continue;
      spokes.push({
        factNodeId: satellite.fact.nodeId,
        hubId: hub.id,
        x1: satellite.x,
        y1: satellite.y,
        x2: hub.x,
        y2: hub.y,
        primary: entityId === satellite.hubId,
      });
    }
  }

  const grouped = new Map<string, { kind: SceneRelation['kind']; from: string; to: string; relations: SceneRelation[] }>();
  const chords: LatticeChord[] = [];
  let relationsIntoAggregates = 0;
  for (const relation of scene.relations) {
    const a = placedAt.get(relation.source);
    const b = placedAt.get(relation.target);
    if (!a || !b) {
      relationsIntoAggregates += 1;
      continue;
    }
    const hubA = primaryOf.get(relation.source)!;
    const hubB = primaryOf.get(relation.target)!;
    if (hubA === hubB) {
      const hub = hubAt.get(hubA)!;
      chords.push({ relation, d: `M ${a.x} ${a.y} Q ${round((a.x + b.x) / 2 * 0.5 + hub.x * 0.5)} ${round((a.y + b.y) / 2 * 0.5 + hub.y * 0.5)} ${b.x} ${b.y}` });
      continue;
    }
    const [from, to] = hubA < hubB ? [hubA, hubB] : [hubB, hubA];
    const key = `${from}|${to}|${relation.kind}`;
    const entry = grouped.get(key) ?? { kind: relation.kind, from, to, relations: [] };
    entry.relations.push(relation);
    grouped.set(key, entry);
  }
  const perPair = new Map<string, SceneRelation['kind'][]>();
  for (const entry of grouped.values()) {
    const pair = `${entry.from}|${entry.to}`;
    perPair.set(pair, [...(perPair.get(pair) ?? []), entry.kind]);
  }
  const bundles: LatticeBundle[] = [...grouped.entries()]
    .sort((a, b) => a[0].localeCompare(b[0]))
    .map(([key, entry]) => {
      const a = hubAt.get(entry.from)!;
      const b = hubAt.get(entry.to)!;
      const kinds = [...(perPair.get(`${entry.from}|${entry.to}`) ?? [])].sort(
        (x, y) => KIND_ORDER.indexOf(x) - KIND_ORDER.indexOf(y),
      );
      const slot = kinds.indexOf(entry.kind) - (kinds.length - 1) / 2;
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const length = Math.max(1, Math.hypot(dx, dy));
      const bend = 0.12 * length + slot * 16;
      const cx = (a.x + b.x) / 2 + (-dy / length) * bend;
      const cy = (a.y + b.y) / 2 + (dx / length) * bend;
      return {
        key,
        kind: entry.kind,
        from: entry.from,
        to: entry.to,
        count: entry.relations.length,
        relations: entry.relations,
        d: `M ${a.x} ${a.y} Q ${round(cx)} ${round(cy)} ${b.x} ${b.y}`,
        midX: round(0.25 * a.x + 0.5 * cx + 0.25 * b.x),
        midY: round(0.25 * a.y + 0.5 * cy + 0.25 * b.y),
        length: round(length),
      };
    });

  return { width: box.width, height: box.height, hubs, satellites, aggregates, bundles, chords, spokes, relationsIntoAggregates };
}

type Mutable = { x: number; y: number; orbit: number };

function fit(hubs: Mutable[], box: { width: number; height: number }) {
  if (hubs.length === 0) return;
  const xs = hubs.map((hub) => hub.x);
  const ys = hubs.map((hub) => hub.y);
  const [minX, maxX, minY, maxY] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
  const w = Math.max(1, box.width - MARGIN * 2);
  const h = Math.max(1, box.height - MARGIN * 2);
  for (const hub of hubs) {
    hub.x = maxX > minX ? MARGIN + ((hub.x - minX) / (maxX - minX)) * w : box.width / 2;
    hub.y = maxY > minY ? MARGIN + ((hub.y - minY) / (maxY - minY)) * h : box.height / 2;
  }
}

/** Push overlapping orbits apart, then clamp into the box. Fixed passes. */
function separate(hubs: Mutable[], box: { width: number; height: number }) {
  for (let pass = 0; pass < 40; pass += 1) {
    let moved = false;
    for (let i = 0; i < hubs.length; i += 1) {
      for (let j = i + 1; j < hubs.length; j += 1) {
        const a = hubs[i]!;
        const b = hubs[j]!;
        const min = a.orbit + b.orbit + 18;
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let d = Math.hypot(dx, dy);
        if (d >= min) continue;
        if (d < 1e-6) {
          dx = 1;
          dy = 0;
          d = 1;
        }
        const push = (min - d) / 2;
        a.x -= (dx / d) * push;
        a.y -= (dy / d) * push;
        b.x += (dx / d) * push;
        b.y += (dy / d) * push;
        moved = true;
      }
    }
    for (const hub of hubs) {
      hub.x = Math.max(hub.orbit + 8, Math.min(box.width - hub.orbit - 8, hub.x));
      hub.y = Math.max(hub.orbit + 8, Math.min(box.height - hub.orbit - 20, hub.y));
    }
    if (!moved) break;
  }
}

function round(value: number): number {
  return Math.round(value * 100) / 100;
}
