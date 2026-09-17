/**
 * FACT CONSTELLATION, the memory graph laid out so that position is a
 * measurement rather than a simulation.
 *
 * The overview route serves one verified memory topology (`holographic.graph`):
 * fact roots, the entities they mention, active assertions, retrieval anchors,
 * and the typed relations between them. This module turns that payload into
 * stable coordinates without a force layout, because a force layout of a
 * bounded slice of an append-only store has no shape to discover, it would
 * rearrange itself on every reload and imply proximity the wire never stated.
 *
 * The composition is polar and every axis is printed by the view:
 *
 *   angle    the fact's category. Categories occupy contiguous sectors sized
 *            by their share of the drawn facts, in a fixed alphabetical order,
 *            so the same store draws the same wheel across reloads.
 *   radius   trust. Fully trusted facts sit at the inner ring and trust falls
 *            outward; a fact whose trust the store did not report sits past
 *            the outer ring, hollow, rather than being given a number.
 *   entities, assertions and anchors have no trust of their own, so they take
 *            the circular mean of the facts they are wired to and sit one step
 *            further out, the relation is what places them, and a node the
 *            graph wired to nothing drawn lands on the rim at an angle derived
 *            from its own id.
 *
 * Nothing here reads a payload over the wire, keeps time, or knows about
 * hover: it is a pure transform of the graph the daemon served, and the same
 * input yields byte-identical output.
 */
import type {
  MemoryGraphEdgeV1,
  MemoryGraphNodeV1,
  MemoryGraphPayloadV1,
  PayloadAccessState,
  ProjectMemoryGraphRelationKindV1,
} from '../../contracts/generated.ts';

/** Logical drawing space. The view maps it onto the aperture with a
 * `viewBox`, so these are proportions rather than device pixels. */
export const CONSTELLATION_WORLD = { width: 1000, height: 600 } as const;
const CENTER = { x: CONSTELLATION_WORLD.width / 2, y: CONSTELLATION_WORLD.height / 2 };
/** Radius of a fact at trust 1.00. */
const RADIUS_TRUSTED = 58;
/** Radius of a fact at trust 0.00. */
const RADIUS_UNTRUSTED = 236;
/** Where a fact with no reported trust sits: past the measured range so it
 * cannot be read as a low score. */
const RADIUS_UNMEASURED = 262;
/** How far outside their facts the wired satellites orbit. */
const SATELLITE_LIFT = 34;
/** Rim for satellites wired to nothing that was drawn; also where the view
 * engraves the category labels. */
export const CONSTELLATION_RIM = 284;
const RADIUS_RIM = CONSTELLATION_RIM;
/** Angular padding inside each category sector, so two sectors' edge facts
 * never sit on the same ray. */
const SECTOR_PADDING = 0.06;
/** A sector never shrinks below this many radians however few facts it holds,
 * so a one-fact category remains a visible wedge rather than a ray. */
const SECTOR_FLOOR = 0.22;
/** The wheel starts at twelve o'clock and runs clockwise. */
const ANGLE_ORIGIN = -Math.PI / 2;

export type ConstellationNodeKind = MemoryGraphNodeV1['kind'];

export interface ConstellationNode {
  /** The graph's own node id (`fact:…`, `entity:…`). */
  readonly id: string;
  readonly kind: ConstellationNodeKind;
  /** Canonical fact identity for fact nodes; `null` for satellites. */
  readonly factId: string | null;
  readonly label: string;
  readonly category: string | null;
  /** Reported trust, or `null` where the store did not report one. */
  readonly trust: number | null;
  readonly payloadAccess: PayloadAccessState | null;
  readonly x: number;
  readonly y: number;
  /** Body radius in world units, trust plus wiring, never decoration. */
  readonly r: number;
  /** How many drawn relations touch this node. */
  readonly degree: number;
  /** Which trust band the node falls in, for the legend and for hollow bodies. */
  readonly band: TrustBandId;
  /** Whether the view should print this node's label. A bounded set of the
   * most wired, most trusted facts carry text; the ledger carries the rest. */
  readonly labelled: boolean;
  /** Which side of the body the label sits on, so text never crosses the hub. */
  readonly labelSide: 'left' | 'right';
}

export interface ConstellationLink {
  readonly id: string;
  readonly source: string;
  readonly target: string;
  readonly kind: ProjectMemoryGraphRelationKindV1;
  readonly x1: number;
  readonly y1: number;
  readonly x2: number;
  readonly y2: number;
}

export interface CategorySector {
  readonly category: string;
  readonly count: number;
  /** Radians, clockwise from twelve o'clock. */
  readonly start: number;
  readonly end: number;
  /** Where the sector's engraved label sits, on the rim. */
  readonly labelX: number;
  readonly labelY: number;
  readonly labelAnchor: 'start' | 'middle' | 'end';
}

export type TrustBandId = 'b80' | 'b60' | 'b40' | 'b20' | 'b00' | 'unmeasured';

export interface TrustBand {
  readonly id: TrustBandId;
  /** Printed range, e.g. `0.80–1.00`. */
  readonly label: string;
  /** Inclusive lower edge; `null` for the unmeasured band. */
  readonly lower: number | null;
  readonly count: number;
  /** World radius of the band's inner edge ring, `null` for unmeasured. */
  readonly ring: number | null;
}

export interface ConstellationCoverage {
  /** Fact nodes drawn. */
  readonly drawnFacts: number;
  /** Satellites (entities, assertions, anchors) drawn. */
  readonly drawnSatellites: number;
  /** Relations drawn, edges whose both ends were among the drawn nodes. */
  readonly drawnRelations: number;
  /** Edges the payload carried but which name a node it did not include. */
  readonly danglingRelations: number;
  /** The daemon's own accounting of the universe this slice came from. */
  readonly factUniverse: number;
  readonly factCandidatesExamined: number;
  readonly unavailableFactCandidates: number;
  readonly rootCount: number;
  readonly relationCount: number;
  readonly relationLimit: number;
  readonly completeness: MemoryGraphPayloadV1['coverage']['completeness'];
  readonly omissionReasons: readonly string[];
}

export interface ConstellationModel {
  readonly nodes: readonly ConstellationNode[];
  readonly links: readonly ConstellationLink[];
  readonly sectors: readonly CategorySector[];
  readonly bands: readonly TrustBand[];
  readonly coverage: ConstellationCoverage;
  /** Relation kinds present in the drawn links, for the relation legend. */
  readonly relationKinds: readonly ProjectMemoryGraphRelationKindV1[];
  /** Adjacency over drawn node ids, for the view's dim-unrelated treatment. */
  readonly neighbours: ReadonlyMap<string, ReadonlySet<string>>;
  /** Drawn node id keyed by canonical fact id, so a ledger row can find its body. */
  readonly nodeIdByFact: ReadonlyMap<string, string>;
}

/** How many facts carry a printed label. */
const LABEL_BUDGET = 7;
/** The footprint one printed label claims, in world units: a 30-character
 * line of 10.5px mono and a row of it. Two labels closer than this on the
 * same side would overprint. */
const LABEL_TEXT_WIDTH = 200;
const LABEL_ROW_HEIGHT = 13;

/** The trust bands the legend prints, highest first. Edges are inclusive at
 * the lower bound so `0.80` reads in the top band. */
const BANDS: readonly { id: TrustBandId; label: string; lower: number }[] = [
  { id: 'b80', label: '0.80–1.00', lower: 0.8 },
  { id: 'b60', label: '0.60–0.79', lower: 0.6 },
  { id: 'b40', label: '0.40–0.59', lower: 0.4 },
  { id: 'b20', label: '0.20–0.39', lower: 0.2 },
  { id: 'b00', label: '0.00–0.19', lower: 0 },
];

export function trustBandOf(trust: number | null): TrustBandId {
  if (trust == null || !Number.isFinite(trust)) return 'unmeasured';
  for (const band of BANDS) {
    if (trust >= band.lower) return band.id;
  }
  return 'b00';
}

/** Trust to radius: 1.00 at the inner ring, 0.00 at the outer, unreported past it. */
export function trustRadius(trust: number | null): number {
  if (trust == null || !Number.isFinite(trust)) return RADIUS_UNMEASURED;
  const clamped = Math.max(0, Math.min(1, trust));
  return RADIUS_TRUSTED + (1 - clamped) * (RADIUS_UNTRUSTED - RADIUS_TRUSTED);
}

function polar(angle: number, radius: number): { x: number; y: number } {
  return {
    x: round(CENTER.x + Math.cos(angle) * radius),
    y: round(CENTER.y + Math.sin(angle) * radius),
  };
}

function round(value: number): number {
  return Math.round(value * 100) / 100;
}

/** A stable angle for a node the graph wired to nothing drawn. FNV-1a over the
 * id: cheap, deterministic, and spread well enough that two orphans rarely
 * share a ray. */
function hashAngle(id: string): number {
  let hash = 0x811c9dc5;
  for (let index = 0; index < id.length; index += 1) {
    hash ^= id.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return ANGLE_ORIGIN + (hash / 0xffffffff) * Math.PI * 2;
}

/** Circular mean of a set of angles, so a satellite between facts at 350° and
 * 10° lands at 0° rather than at 180°. */
function meanAngle(angles: readonly number[]): number {
  let x = 0;
  let y = 0;
  for (const angle of angles) {
    x += Math.cos(angle);
    y += Math.sin(angle);
  }
  return Math.atan2(y, x);
}

interface FactSeed {
  node: Extract<MemoryGraphNodeV1, { kind: 'fact' }>;
  category: string;
  trust: number | null;
}

const UNCATEGORISED = 'uncategorised';

/** Lay the graph out. Pure; the same payload yields the same model. */
export function composeConstellation(graph: MemoryGraphPayloadV1): ConstellationModel {
  const factSeeds: FactSeed[] = [];
  const satellites: Exclude<MemoryGraphNodeV1, { kind: 'fact' }>[] = [];
  for (const node of graph.nodes) {
    if (node.kind === 'fact') {
      factSeeds.push({
        node,
        category: node.category ?? UNCATEGORISED,
        trust:
          typeof node.trust_score === 'number' && Number.isFinite(node.trust_score)
            ? node.trust_score
            : null,
      });
    } else {
      satellites.push(node);
    }
  }

  // Sectors: alphabetical categories, each a share of the wheel by count with
  // a floor so a sparse category is still a wedge.
  const byCategory = new Map<string, FactSeed[]>();
  for (const seed of factSeeds) {
    const bucket = byCategory.get(seed.category) ?? [];
    bucket.push(seed);
    byCategory.set(seed.category, bucket);
  }
  const categories = [...byCategory.keys()].sort((a, b) => a.localeCompare(b));
  const rawShares = categories.map((category) =>
    Math.max(SECTOR_FLOOR, ((byCategory.get(category)?.length ?? 0) / Math.max(1, factSeeds.length)) * Math.PI * 2),
  );
  const shareTotal = rawShares.reduce((sum, share) => sum + share, 0);
  const sectors: CategorySector[] = [];
  let cursor = ANGLE_ORIGIN;
  categories.forEach((category, index) => {
    const span = shareTotal > 0 ? ((rawShares[index] ?? 0) / shareTotal) * Math.PI * 2 : 0;
    const start = cursor;
    const end = cursor + span;
    cursor = end;
    const mid = (start + end) / 2;
    const label = polar(mid, RADIUS_RIM + 14);
    const cos = Math.cos(mid);
    sectors.push({
      category,
      count: byCategory.get(category)?.length ?? 0,
      start,
      end,
      labelX: label.x,
      labelY: label.y,
      labelAnchor: cos > 0.25 ? 'start' : cos < -0.25 ? 'end' : 'middle',
    });
  });

  // Facts: within a sector, trust descending then id, spread evenly across the
  // padded sector. Because radius grows as trust falls, the sequence traces a
  // spiral outward and adjacent bodies differ on both axes.
  const positioned = new Map<string, { x: number; y: number; angle: number; radius: number }>();
  const angleOf = new Map<string, number>();
  for (const sector of sectors) {
    const seeds = [...(byCategory.get(sector.category) ?? [])].sort(
      (a, b) =>
        (b.trust ?? -1) - (a.trust ?? -1) || a.node.fact_id.localeCompare(b.node.fact_id),
    );
    const usableStart = sector.start + SECTOR_PADDING;
    const usableEnd = sector.end - SECTOR_PADDING;
    const step = seeds.length > 1 ? (usableEnd - usableStart) / (seeds.length - 1) : 0;
    seeds.forEach((seed, index) => {
      const angle = seeds.length > 1 ? usableStart + step * index : (sector.start + sector.end) / 2;
      const radius = trustRadius(seed.trust);
      const point = polar(angle, radius);
      positioned.set(seed.node.id, { ...point, angle, radius });
      angleOf.set(seed.node.id, angle);
    });
  }

  // Degree over edges whose both ends are drawn. Dangling edges are counted,
  // not drawn: a line to a body that is not there is a line to nothing.
  const drawnIds = new Set<string>([
    ...factSeeds.map((seed) => seed.node.id),
    ...satellites.map((node) => node.id),
  ]);
  const degree = new Map<string, number>();
  const neighbours = new Map<string, Set<string>>();
  const drawnEdges: MemoryGraphEdgeV1[] = [];
  let dangling = 0;
  for (const edge of graph.edges) {
    if (!drawnIds.has(edge.source) || !drawnIds.has(edge.target)) {
      dangling += 1;
      continue;
    }
    drawnEdges.push(edge);
    degree.set(edge.source, (degree.get(edge.source) ?? 0) + 1);
    degree.set(edge.target, (degree.get(edge.target) ?? 0) + 1);
    let sourceSet = neighbours.get(edge.source);
    if (!sourceSet) {
      sourceSet = new Set();
      neighbours.set(edge.source, sourceSet);
    }
    sourceSet.add(edge.target);
    let targetSet = neighbours.get(edge.target);
    if (!targetSet) {
      targetSet = new Set();
      neighbours.set(edge.target, targetSet);
    }
    targetSet.add(edge.source);
  }

  // Satellites: circular mean of wired fact angles, one step past the mean
  // fact radius; the rim at a hashed angle when wired to nothing drawn.
  for (const satellite of satellites) {
    const wiredFacts = [...(neighbours.get(satellite.id) ?? [])]
      .map((id) => positioned.get(id))
      .filter((point): point is NonNullable<typeof point> => point !== undefined);
    if (wiredFacts.length === 0) {
      const angle = hashAngle(satellite.id);
      positioned.set(satellite.id, { ...polar(angle, RADIUS_RIM), angle, radius: RADIUS_RIM });
      continue;
    }
    const angle = meanAngle(wiredFacts.map((point) => point.angle));
    const meanRadius =
      wiredFacts.reduce((sum, point) => sum + point.radius, 0) / wiredFacts.length;
    const radius = Math.min(RADIUS_RIM, meanRadius + SATELLITE_LIFT);
    positioned.set(satellite.id, { ...polar(angle, radius), angle, radius });
  }

  // Labels: the budget goes to the most wired facts, ties to the most trusted,
  // and a candidate whose text would sit on top of an accepted label's text
  // yields its place to the next one down. Greedy and deterministic: the
  // same graph labels the same bodies.
  const labelCandidates = [...factSeeds].sort(
    (a, b) =>
      (degree.get(b.node.id) ?? 0) - (degree.get(a.node.id) ?? 0) ||
      (b.trust ?? -1) - (a.trust ?? -1) ||
      a.node.fact_id.localeCompare(b.node.fact_id),
  );
  const accepted: { x: number; y: number; side: 'left' | 'right' }[] = [];
  const labelled = new Set<string>();
  for (const seed of labelCandidates) {
    if (labelled.size >= LABEL_BUDGET) break;
    const point = positioned.get(seed.node.id);
    if (!point) continue;
    const side: 'left' | 'right' = Math.cos(point.angle) >= 0 ? 'right' : 'left';
    const collides = accepted.some(
      (other) =>
        Math.abs(other.y - point.y) < LABEL_ROW_HEIGHT &&
        (other.side === side
          ? Math.abs(other.x - point.x) < LABEL_TEXT_WIDTH
          : side === 'right'
            ? point.x < other.x && other.x - point.x < LABEL_TEXT_WIDTH
            : point.x > other.x && point.x - other.x < LABEL_TEXT_WIDTH),
    );
    if (collides) continue;
    accepted.push({ x: point.x, y: point.y, side });
    labelled.add(seed.node.id);
  }

  const bandCounts = new Map<TrustBandId, number>();
  const nodes: ConstellationNode[] = [];
  const nodeIdByFact = new Map<string, string>();
  for (const seed of factSeeds) {
    const point = positioned.get(seed.node.id);
    if (!point) continue;
    const band = trustBandOf(seed.trust);
    bandCounts.set(band, (bandCounts.get(band) ?? 0) + 1);
    const wired = degree.get(seed.node.id) ?? 0;
    nodeIdByFact.set(seed.node.fact_id, seed.node.id);
    nodes.push({
      id: seed.node.id,
      kind: 'fact',
      factId: seed.node.fact_id,
      label: seed.node.label,
      category: seed.node.category,
      trust: seed.trust,
      payloadAccess: seed.node.payload_access,
      x: point.x,
      y: point.y,
      r: round(3.2 + (seed.trust ?? 0) * 3.4 + Math.min(wired, 6) * 0.55),
      degree: wired,
      band,
      labelled: labelled.has(seed.node.id),
      labelSide: Math.cos(point.angle) >= 0 ? 'right' : 'left',
    });
  }
  for (const satellite of satellites) {
    const point = positioned.get(satellite.id);
    if (!point) continue;
    const wired = degree.get(satellite.id) ?? 0;
    nodes.push({
      id: satellite.id,
      kind: satellite.kind,
      factId: null,
      label: satellite.label,
      category: null,
      trust: null,
      payloadAccess: null,
      x: point.x,
      y: point.y,
      r: round(2 + Math.min(wired, 8) * 0.45),
      degree: wired,
      band: 'unmeasured',
      labelled: false,
      labelSide: Math.cos(point.angle) >= 0 ? 'right' : 'left',
    });
  }

  const links: ConstellationLink[] = drawnEdges.map((edge, index) => {
    const source = positioned.get(edge.source);
    const target = positioned.get(edge.target);
    return {
      id: `${edge.kind}:${edge.source}→${edge.target}:${index}`,
      source: edge.source,
      target: edge.target,
      kind: edge.kind,
      x1: source?.x ?? CENTER.x,
      y1: source?.y ?? CENTER.y,
      x2: target?.x ?? CENTER.x,
      y2: target?.y ?? CENTER.y,
    };
  });

  const bands: TrustBand[] = [
    ...BANDS.map((band) => ({
      id: band.id,
      label: band.label,
      lower: band.lower,
      count: bandCounts.get(band.id) ?? 0,
      ring: round(trustRadius(band.lower)),
    })),
    {
      id: 'unmeasured',
      label: 'unmeasured',
      lower: null,
      count: bandCounts.get('unmeasured') ?? 0,
      ring: null,
    },
  ];

  const relationKinds = [...new Set(links.map((link) => link.kind))].sort((a, b) =>
    a.localeCompare(b),
  );

  return {
    nodes,
    links,
    sectors,
    bands,
    relationKinds,
    neighbours,
    nodeIdByFact,
    coverage: {
      drawnFacts: factSeeds.length,
      drawnSatellites: satellites.length,
      drawnRelations: links.length,
      danglingRelations: dangling,
      factUniverse: graph.fact_universe_count,
      factCandidatesExamined: graph.fact_candidates_examined,
      unavailableFactCandidates: graph.unavailable_fact_candidates,
      rootCount: graph.root_count,
      relationCount: graph.relation_count,
      relationLimit: graph.relation_limit,
      completeness: graph.coverage.completeness,
      omissionReasons: graph.coverage.omission_reasons,
    },
  };
}

/** The relation kinds, as the view draws and the legend names them. Line
 * style carries the kind so it survives without colour. */
export function relationStyle(kind: ProjectMemoryGraphRelationKindV1): {
  label: string;
  dash: string | undefined;
  tone: 'signal' | 'conflict' | 'stale' | 'quiet';
} {
  switch (kind) {
    case 'supports':
      return { label: 'supports', dash: undefined, tone: 'signal' };
    case 'contradicts':
      return { label: 'contradicts', dash: '5 4', tone: 'conflict' };
    case 'supersedes':
      return { label: 'supersedes', dash: '7 3 1.5 3', tone: 'stale' };
    case 'derived_from':
      return { label: 'derived from', dash: '1.5 3.5', tone: 'signal' };
    case 'mentions':
      return { label: 'mentions', dash: undefined, tone: 'quiet' };
    case 'active_assertion':
      return { label: 'active assertion', dash: undefined, tone: 'quiet' };
    case 'evidence_anchor':
      return { label: 'evidence anchor', dash: '1.5 3.5', tone: 'quiet' };
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** How a satellite kind is drawn and named. Facts are discs; everything else
 * takes a distinct glyph so kind is never carried by size alone. */
export function nodeGlyph(kind: ConstellationNodeKind): {
  label: string;
  shape: 'disc' | 'diamond' | 'square' | 'tick';
} {
  switch (kind) {
    case 'fact':
      return { label: 'fact', shape: 'disc' };
    case 'entity':
      return { label: 'entity', shape: 'diamond' };
    case 'assertion':
      return { label: 'assertion', shape: 'square' };
    case 'retrieval_anchor':
      return { label: 'retrieval anchor', shape: 'tick' };
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** The accessible reading of the constellation: what is drawn, on which axes,
 * from which slice, and where the exact rows are. A summary of the
 * measurements, never a narration of pixels. */
export function constellationDescription(model: ConstellationModel): string {
  const { coverage } = model;
  const occupied = model.bands
    .filter((band) => band.count > 0)
    .map((band) => `${band.count} at ${band.label}`)
    .join(', ');
  const sectors = model.sectors.map((sector) => `${sector.category} ${sector.count}`).join(', ');
  const relations =
    coverage.drawnRelations === 0
      ? 'no relation was returned to draw'
      : `${coverage.drawnRelations} ${coverage.drawnRelations === 1 ? 'relation' : 'relations'} drawn (${model.relationKinds.join(', ')})`;
  return (
    `Fact constellation: ${coverage.drawnFacts} fact ${coverage.drawnFacts === 1 ? 'root' : 'roots'} placed by category around the wheel (${sectors || 'none'}) and by trust toward the centre (${occupied || 'none measured'}); ` +
    `${coverage.drawnSatellites} wired ${coverage.drawnSatellites === 1 ? 'satellite' : 'satellites'}; ${relations}. ` +
    `Drawn from ${coverage.factCandidatesExamined} examined of ${coverage.factUniverse} facts in the store, graph coverage ${coverage.completeness}. ` +
    `The fact ledger beside this field is the exact accessible equivalent.`
  );
}
