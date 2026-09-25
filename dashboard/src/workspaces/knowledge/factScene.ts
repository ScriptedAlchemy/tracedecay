/**
 * The fact scene the Facts camera draws.
 *
 * It joins the verified memory topology the overview served (`graph`) to the
 * bounded fact rows served beside it (`facts`). A fact the graph drew but the
 * bounded rows did not include keeps its node readings and prints its row
 * readings as absent; nothing is borrowed from elsewhere to fill it.
 *
 * Entities are joined by `mentions` edges only. A mention names an entity the
 * fact cites, not a resolved code symbol, so the drawing states that the
 * grouping is by name.
 *
 * A relation is drawn only when both of its ends are in the payload; an edge
 * naming a node the bounded slice did not include is counted as dangling,
 * never drawn to nothing. Pure: the same payload yields the same scene.
 */
import type {
  MemoryFactRowV1,
  MemoryGraphPayloadV1,
  PayloadAccessState,
  ProjectMemoryGraphRelationKindV1,
} from '../../contracts/generated.ts';

export type TrustBandId = 'b80' | 'b60' | 'b40' | 'b20' | 'b00' | 'unmeasured';

/** Edges are inclusive at the lower bound so `0.80` reads in the top band. */
const BANDS: readonly { id: TrustBandId; lower: number }[] = [
  { id: 'b80', lower: 0.8 },
  { id: 'b60', lower: 0.6 },
  { id: 'b40', lower: 0.4 },
  { id: 'b20', lower: 0.2 },
  { id: 'b00', lower: 0 },
];

export function trustBandOf(trust: number | null): TrustBandId {
  if (trust == null || !Number.isFinite(trust)) return 'unmeasured';
  return BANDS.find((band) => trust >= band.lower)?.id ?? 'b00';
}

/** How a relation kind is drawn and named. Line style carries the kind so it
 * survives without colour. */
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

export interface SceneFact {
  readonly nodeId: string;
  readonly factId: string;
  /** Printable label. A withheld fact prints its access state and identity
   * tail, never content. */
  readonly label: string;
  readonly category: string | null;
  readonly trust: number | null;
  readonly band: TrustBandId;
  readonly access: PayloadAccessState;
  readonly restricted: boolean;
  readonly retrievals: number | null;
  /** Row `updated_at` in microseconds, `null` when absent or the row was not loaded. */
  readonly updatedAt: number | null;
  readonly rowLoaded: boolean;
  /** Entity node ids this fact mentions, sorted. */
  readonly entityIds: readonly string[];
  /** Drawn relations touching the fact, mentions included. */
  readonly degree: number;
}

export interface SceneEntity {
  readonly nodeId: string;
  readonly label: string;
  /** Fact node ids mentioning this entity, trust descending. */
  readonly factIds: readonly string[];
}

/** A fact-to-fact relation; mentions are carried on the facts instead. */
export interface SceneRelation {
  readonly id: string;
  readonly kind: Exclude<ProjectMemoryGraphRelationKindV1, 'mentions'>;
  readonly source: string;
  readonly target: string;
}

export interface SceneCoverage {
  readonly drawnFacts: number;
  /** Relations with both ends in the payload, mentions included. */
  readonly drawnRelations: number;
  /** Edges naming a node the payload did not include. */
  readonly danglingRelations: number;
  readonly factUniverse: number;
  readonly factCandidatesExamined: number;
  readonly unavailableFactCandidates: number;
  /** Withheld facts the payload still drew as roots, identity only. */
  readonly withheldDrawn: number;
  readonly relationCount: number;
  readonly relationLimit: number;
  readonly completeness: MemoryGraphPayloadV1['coverage']['completeness'];
  readonly omissionReasons: readonly string[];
}

export interface FactScene {
  readonly facts: readonly SceneFact[];
  readonly entities: readonly SceneEntity[];
  readonly relations: readonly SceneRelation[];
  /** Assertions and retrieval anchors, which this drawing does not place, counted. */
  readonly unplaced: number;
  /** Relations between a fact and an assertion or anchor, counted. */
  readonly unplacedRelations: number;
  readonly coverage: SceneCoverage;
  /** Adjacency over every drawn relation, for the dim-unrelated treatment. */
  readonly neighbours: ReadonlyMap<string, ReadonlySet<string>>;
  readonly byNode: ReadonlyMap<string, SceneFact>;
  /** Drawn node id keyed by canonical fact id, so a ledger row can find its mark. */
  readonly nodeIdByFact: ReadonlyMap<string, string>;
}

export function accessWord(access: PayloadAccessState): string {
  return access.replaceAll('_', ' ');
}

/** The identity tail a withheld fact prints: enough to tell two apart. */
export function factIdTail(factId: string): string {
  return factId.length > 8 ? `…${factId.slice(-6)}` : factId;
}

export function composeFactScene(graph: MemoryGraphPayloadV1, rows: readonly MemoryFactRowV1[]): FactScene {
  const rowById = new Map(rows.map((row) => [row.fact_id, row]));
  const kindOf = new Map(graph.nodes.map((node) => [node.id, node.kind]));

  const degree = new Map<string, number>();
  const neighbours = new Map<string, Set<string>>();
  const link = (from: string, to: string) => {
    degree.set(from, (degree.get(from) ?? 0) + 1);
    const set = neighbours.get(from) ?? new Set<string>();
    set.add(to);
    neighbours.set(from, set);
  };
  const mentions = new Map<string, Set<string>>();
  const relations: SceneRelation[] = [];
  let dangling = 0;
  let drawnRelations = 0;
  let unplacedRelations = 0;
  graph.edges.forEach((edge, index) => {
    const source = kindOf.get(edge.source);
    const target = kindOf.get(edge.target);
    if (source === undefined || target === undefined) {
      dangling += 1;
      return;
    }
    drawnRelations += 1;
    link(edge.source, edge.target);
    link(edge.target, edge.source);
    if (edge.kind === 'mentions') {
      const fact = source === 'fact' ? edge.source : target === 'fact' ? edge.target : null;
      const entity = source === 'entity' ? edge.source : target === 'entity' ? edge.target : null;
      if (fact && entity) {
        const set = mentions.get(fact) ?? new Set<string>();
        set.add(entity);
        mentions.set(fact, set);
      } else unplacedRelations += 1;
      return;
    }
    if (source === 'fact' && target === 'fact') {
      relations.push({
        id: `${edge.kind}:${edge.source}→${edge.target}:${index}`,
        kind: edge.kind,
        source: edge.source,
        target: edge.target,
      });
    } else unplacedRelations += 1;
  });

  const facts: SceneFact[] = [];
  let unplaced = 0;
  for (const node of graph.nodes) {
    if (node.kind === 'assertion' || node.kind === 'retrieval_anchor') unplaced += 1;
    if (node.kind !== 'fact') continue;
    const row = rowById.get(node.fact_id);
    const trust = typeof node.trust_score === 'number' && Number.isFinite(node.trust_score) ? node.trust_score : null;
    const restricted = node.payload_access !== 'eligible';
    facts.push({
      nodeId: node.id,
      factId: node.fact_id,
      label: restricted ? `${accessWord(node.payload_access)} · ${factIdTail(node.fact_id)}` : node.label,
      category: node.category,
      trust,
      band: trustBandOf(trust),
      access: node.payload_access,
      restricted,
      retrievals: node.retrieval_count,
      updatedAt: row?.updated_at ?? null,
      rowLoaded: row !== undefined,
      entityIds: [...(mentions.get(node.id) ?? [])].sort((a, b) => a.localeCompare(b)),
      degree: degree.get(node.id) ?? 0,
    });
  }
  const byNode = new Map(facts.map((fact) => [fact.nodeId, fact]));

  const entities: SceneEntity[] = graph.nodes
    .filter((node) => node.kind === 'entity')
    .map((node) => ({
      nodeId: node.id,
      label: node.label,
      factIds: facts
        .filter((fact) => fact.entityIds.includes(node.id))
        .sort((a, b) => (b.trust ?? -1) - (a.trust ?? -1) || a.factId.localeCompare(b.factId))
        .map((fact) => fact.nodeId),
    }))
    .sort((a, b) => b.factIds.length - a.factIds.length || a.label.localeCompare(b.label));

  return {
    facts,
    entities,
    relations,
    unplaced,
    unplacedRelations,
    coverage: {
      drawnFacts: facts.length,
      drawnRelations,
      danglingRelations: dangling,
      factUniverse: graph.fact_universe_count,
      factCandidatesExamined: graph.fact_candidates_examined,
      unavailableFactCandidates: graph.unavailable_fact_candidates,
      withheldDrawn: facts.filter((fact) => fact.restricted).length,
      relationCount: graph.relation_count,
      relationLimit: graph.relation_limit,
      completeness: graph.coverage.completeness,
      omissionReasons: graph.coverage.omission_reasons,
    },
    neighbours,
    byNode,
    nodeIdByFact: new Map(facts.map((fact) => [fact.factId, fact.nodeId])),
  };
}

/** The fact a focal readout names when nothing is selected or inspected: the
 * most wired readable fact, ties to the most trusted, then identity. */
export function hubFact(scene: FactScene): SceneFact | null {
  return (
    [...scene.facts]
      .filter((fact) => !fact.restricted)
      .sort((a, b) => b.degree - a.degree || (b.trust ?? -1) - (a.trust ?? -1) || a.factId.localeCompare(b.factId))[0] ??
    null
  );
}

/** Contradictions and supersessions, the relations the camera enumerates. */
export function disputesOf(scene: FactScene): readonly SceneRelation[] {
  return scene.relations.filter((relation) => relation.kind === 'contradicts' || relation.kind === 'supersedes');
}

/** Average glyph advance, in ems, of the faces the camera sets text in. The
 * layout budgets text with these; the browser still sets the real glyphs. */
export const ADVANCE = { body: 0.53, mono: 0.6, legend: 0.78 } as const;

export function elideToWidth(text: string, widthPx: number, fontPx: number, advance: number = ADVANCE.mono): string {
  const line = text.split('\n')[0] ?? '';
  const chars = Math.max(1, Math.floor(widthPx / (fontPx * advance)));
  return line.length > chars ? `${line.slice(0, Math.max(1, chars - 1))}…` : line;
}
