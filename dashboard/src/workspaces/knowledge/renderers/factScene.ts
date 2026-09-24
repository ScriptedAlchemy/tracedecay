/**
 * The renderer-neutral fact scene every constellation variant draws.
 *
 * It joins the verified memory topology the overview served (`graph`) to the
 * bounded fact rows served beside it (`facts`), so a renderer can place a fact
 * on a measured axis the graph node does not carry (the row's `updated_at`).
 * A fact the graph drew but the bounded rows did not include keeps its node
 * readings and prints the row readings as absent; nothing is borrowed from
 * elsewhere to fill it.
 *
 * Entities are joined by `mentions` edges only. A mention names an entity the
 * fact cites, not a resolved code symbol, so every renderer states that the
 * grouping is by name.
 */
import type {
  MemoryFactRowV1,
  MemoryGraphPayloadV1,
  PayloadAccessState,
  ProjectMemoryGraphRelationKindV1,
} from '../../../contracts/generated.ts';
import { composeConstellation, trustBandOf, type ConstellationModel, type TrustBandId } from '../constellation.ts';

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
  readonly helpful: number | null;
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

export interface FactScene {
  readonly facts: readonly SceneFact[];
  readonly entities: readonly SceneEntity[];
  readonly relations: readonly SceneRelation[];
  /** Assertions and retrieval anchors drawn by no variant, counted. */
  readonly unplaced: number;
  /** Relations between a fact and a non-fact, non-entity node, counted. */
  readonly unplacedRelations: number;
  /** The polar model supplies the shared coverage, adjacency and fact index. */
  readonly model: ConstellationModel;
  readonly byNode: ReadonlyMap<string, SceneFact>;
}

export function accessWord(access: PayloadAccessState): string {
  return access.replaceAll('_', ' ');
}

/** The identity tail a withheld fact prints: enough to tell two apart. */
export function factIdTail(factId: string): string {
  return factId.length > 8 ? `…${factId.slice(-6)}` : factId;
}

export function composeFactScene(
  graph: MemoryGraphPayloadV1,
  rows: readonly MemoryFactRowV1[],
): FactScene {
  const model = composeConstellation(graph);
  const rowById = new Map(rows.map((row) => [row.fact_id, row]));
  const kindOf = new Map(graph.nodes.map((node) => [node.id, node.kind]));
  const labelOf = new Map(graph.nodes.map((node) => [node.id, node.label]));
  const degreeOf = new Map(model.nodes.map((node) => [node.id, node.degree]));

  const mentions = new Map<string, Set<string>>();
  const relations: SceneRelation[] = [];
  let unplacedRelations = 0;
  graph.edges.forEach((edge, index) => {
    const source = kindOf.get(edge.source);
    const target = kindOf.get(edge.target);
    if (source === undefined || target === undefined) return;
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
      relations.push({ id: `${edge.kind}:${edge.source}→${edge.target}:${index}`, kind: edge.kind, source: edge.source, target: edge.target });
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
      helpful: node.helpful_count,
      updatedAt: row?.updated_at ?? null,
      rowLoaded: row !== undefined,
      entityIds: [...(mentions.get(node.id) ?? [])].sort((a, b) => a.localeCompare(b)),
      degree: degreeOf.get(node.id) ?? 0,
    });
  }
  const byNode = new Map(facts.map((fact) => [fact.nodeId, fact]));

  const entities: SceneEntity[] = graph.nodes
    .filter((node) => node.kind === 'entity')
    .map((node) => ({
      nodeId: node.id,
      label: labelOf.get(node.id) ?? node.id,
      factIds: facts
        .filter((fact) => fact.entityIds.includes(node.id))
        .sort((a, b) => (b.trust ?? -1) - (a.trust ?? -1) || a.factId.localeCompare(b.factId))
        .map((fact) => fact.nodeId),
    }))
    .sort((a, b) => b.factIds.length - a.factIds.length || a.label.localeCompare(b.label));

  return { facts, entities, relations, unplaced, unplacedRelations, model, byNode };
}

/** The fact a focal readout names when nothing is selected or inspected: the
 * most wired, ties to the most trusted, then identity. */
export function hubFact(scene: FactScene): SceneFact | null {
  return (
    [...scene.facts]
      .filter((fact) => !fact.restricted)
      .sort(
        (a, b) =>
          b.degree - a.degree || (b.trust ?? -1) - (a.trust ?? -1) || a.factId.localeCompare(b.factId),
      )[0] ?? null
  );
}

/** Mono glyph advance at `fontPx`, for eliding labels to a pixel budget. */
export const MONO_ADVANCE = 0.6;

export function elideToWidth(text: string, widthPx: number, fontPx: number): string {
  const line = text.split('\n')[0] ?? '';
  const chars = Math.max(1, Math.floor(widthPx / (fontPx * MONO_ADVANCE)));
  return line.length > chars ? `${line.slice(0, Math.max(1, chars - 1))}…` : line;
}
