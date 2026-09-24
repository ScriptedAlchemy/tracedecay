import { describe, expect, it } from 'vitest';

import type { MemoryGraphNodeV1, MemoryGraphPayloadV1 } from '../../contracts/generated.ts';
import {
  CONSTELLATION_WORLD,
  composeConstellation,
  constellationDescription,
  nodeGlyph,
  relationStyle,
  trustBandOf,
  trustRadius,
} from './constellation.ts';

const CENTER = { x: CONSTELLATION_WORLD.width / 2, y: CONSTELLATION_WORLD.height / 2 };

function factNode(
  id: string,
  over: Partial<Extract<MemoryGraphNodeV1, { kind: 'fact' }>> = {},
): Extract<MemoryGraphNodeV1, { kind: 'fact' }> {
  return {
    id: `fact:${id}`,
    kind: 'fact',
    label: `content of ${id}`,
    fact_id: id,
    payload_access: 'eligible',
    projected_as_of: 1,
    content: `content of ${id}`,
    category: 'general',
    trust_score: 0.5,
    retrieval_count: 0,
    helpful_count: 0,
    ...over,
  };
}

function graph(
  nodes: MemoryGraphNodeV1[],
  edges: MemoryGraphPayloadV1['edges'] = [],
  over: Partial<MemoryGraphPayloadV1> = {},
): MemoryGraphPayloadV1 {
  return {
    nodes,
    edges,
    coverage: {
      completeness: 'complete',
      eligible: nodes.length,
      examined: nodes.length,
      matched: nodes.length,
      excluded: 0,
      omitted: 0,
      unknown: 0,
      denominator: nodes.length,
      unit: 'memory_graph_roots',
      omission_reasons: [],
    },
    fact_universe_count: nodes.length,
    fact_candidates_examined: nodes.length,
    unavailable_fact_candidates: 0,
    root_count: nodes.filter((node) => node.kind === 'fact').length,
    relation_limit: 100,
    relation_count: edges.length,
    ...over,
  };
}

function distance(a: { x: number; y: number }, b: { x: number; y: number }): number {
  return Math.hypot(a.x - b.x, a.y - b.y);
}

describe('composeConstellation', () => {
  it('is deterministic: the same payload yields identical coordinates', () => {
    const payload = graph(
      [
        factNode('a', { trust_score: 0.9, category: 'decision' }),
        factNode('b', { trust_score: 0.3, category: 'tool' }),
        { id: 'entity:x', kind: 'entity', entity_id: 'x', label: 'X' },
      ],
      [{ kind: 'mentions', source: 'fact:a', target: 'entity:x' }],
    );
    const first = composeConstellation(payload);
    const second = composeConstellation(structuredClone(payload));
    expect(second.nodes).toEqual(first.nodes);
    expect(second.links).toEqual(first.links);
    expect(second.sectors).toEqual(first.sectors);
  });

  it('places higher trust closer to the centre and unmeasured trust past the outer ring', () => {
    const model = composeConstellation(
      graph([
        factNode('high', { trust_score: 0.95 }),
        factNode('mid', { trust_score: 0.5 }),
        factNode('low', { trust_score: 0.05 }),
        factNode('none', { trust_score: null }),
      ]),
    );
    const radius = (id: string) => {
      const node = model.nodes.find((candidate) => candidate.factId === id);
      if (!node) throw new Error(`missing ${id}`);
      return distance(node, CENTER);
    };
    expect(radius('high')).toBeLessThan(radius('mid'));
    expect(radius('mid')).toBeLessThan(radius('low'));
    expect(radius('low')).toBeLessThan(radius('none'));
    expect(radius('none')).toBeCloseTo(trustRadius(null), 0);
    expect(model.nodes.find((node) => node.factId === 'none')?.band).toBe('unmeasured');
  });

  it('groups facts of one category into one contiguous sector, ordered alphabetically', () => {
    const model = composeConstellation(
      graph([
        factNode('t1', { category: 'tool' }),
        factNode('d1', { category: 'decision' }),
        factNode('t2', { category: 'tool' }),
        factNode('d2', { category: 'decision' }),
        factNode('g1', { category: 'general' }),
      ]),
    );
    expect(model.sectors.map((sector) => sector.category)).toEqual([
      'decision',
      'general',
      'tool',
    ]);
    expect(model.sectors.map((sector) => sector.count)).toEqual([2, 1, 2]);
    // Sectors tile the wheel without gaps or overlap.
    for (let index = 1; index < model.sectors.length; index += 1) {
      expect(model.sectors[index]!.start).toBeCloseTo(model.sectors[index - 1]!.end, 6);
    }
    const first = model.sectors[0]!;
    const last = model.sectors.at(-1)!;
    expect(last.end - first.start).toBeCloseTo(Math.PI * 2, 6);
    // Every fact's angle falls inside its own category's sector.
    for (const node of model.nodes) {
      if (node.kind !== 'fact') continue;
      const sector = model.sectors.find((candidate) => candidate.category === node.category)!;
      let angle = Math.atan2(node.y - CENTER.y, node.x - CENTER.x);
      while (angle < sector.start) angle += Math.PI * 2;
      expect(angle).toBeGreaterThanOrEqual(sector.start - 1e-6);
      expect(angle).toBeLessThanOrEqual(sector.end + 1e-6);
    }
  });

  it('files a fact with no category under an explicit uncategorised sector rather than dropping it', () => {
    const model = composeConstellation(graph([factNode('n', { category: null })]));
    expect(model.sectors.map((sector) => sector.category)).toEqual(['uncategorised']);
    expect(model.nodes).toHaveLength(1);
  });

  it('anchors a wired satellite beside the facts that mention it, one step further out', () => {
    const model = composeConstellation(
      graph(
        [
          factNode('a', { trust_score: 0.8, category: 'decision' }),
          factNode('b', { trust_score: 0.8, category: 'decision' }),
          { id: 'entity:x', kind: 'entity', entity_id: 'x', label: 'X' },
        ],
        [
          { kind: 'mentions', source: 'fact:a', target: 'entity:x' },
          { kind: 'mentions', source: 'fact:b', target: 'entity:x' },
        ],
      ),
    );
    const entity = model.nodes.find((node) => node.kind === 'entity')!;
    const a = model.nodes.find((node) => node.factId === 'a')!;
    const b = model.nodes.find((node) => node.factId === 'b')!;
    const entityAngle = Math.atan2(entity.y - CENTER.y, entity.x - CENTER.x);
    const aAngle = Math.atan2(a.y - CENTER.y, a.x - CENTER.x);
    const bAngle = Math.atan2(b.y - CENTER.y, b.x - CENTER.x);
    expect(entityAngle).toBeGreaterThanOrEqual(Math.min(aAngle, bAngle) - 1e-6);
    expect(entityAngle).toBeLessThanOrEqual(Math.max(aAngle, bAngle) + 1e-6);
    expect(distance(entity, CENTER)).toBeGreaterThan(distance(a, CENTER));
    expect(entity.degree).toBe(2);
    expect(model.links).toHaveLength(2);
    expect(model.neighbours.get('entity:x')).toEqual(new Set(['fact:a', 'fact:b']));
    expect(model.nodeIdByFact.get('a')).toBe('fact:a');
  });

  it('parks a satellite wired to nothing drawn on the rim and counts the dangling relation', () => {
    const model = composeConstellation(
      graph(
        [factNode('a'), { id: 'entity:lonely', kind: 'entity', entity_id: 'lonely', label: 'L' }],
        [{ kind: 'mentions', source: 'fact:missing', target: 'entity:lonely' }],
      ),
    );
    const lonely = model.nodes.find((node) => node.kind === 'entity')!;
    expect(distance(lonely, CENTER)).toBeGreaterThan(trustRadius(null));
    expect(model.links).toHaveLength(0);
    expect(model.coverage.danglingRelations).toBe(1);
    expect(model.coverage.drawnRelations).toBe(0);
  });

  it('labels a bounded set of the most wired facts and never a satellite', () => {
    const nodes: MemoryGraphNodeV1[] = [];
    const edges: MemoryGraphPayloadV1['edges'] = [];
    for (let index = 0; index < 20; index += 1) {
      nodes.push(factNode(`f${index}`, { trust_score: index / 20 }));
    }
    nodes.push({ id: 'entity:hub', kind: 'entity', entity_id: 'hub', label: 'hub' });
    for (let index = 0; index < 3; index += 1) {
      edges.push({ kind: 'mentions', source: `fact:f${index}`, target: 'entity:hub' });
    }
    const model = composeConstellation(graph(nodes, edges));
    const labelled = model.nodes.filter((node) => node.labelled);
    expect(labelled.length).toBeLessThanOrEqual(7);
    expect(labelled.every((node) => node.kind === 'fact')).toBe(true);
    // The three wired facts take the first three labels.
    expect(new Set(labelled.slice(0, 3).map((node) => node.factId))).toEqual(
      new Set(['f0', 'f1', 'f2']),
    );
  });

  it('yields a label whose text would overprint an accepted label to the next candidate', () => {
    // Forty equally trusted facts in one category sit on one ring, thirteen
    // world units apart, all wired to one hub: every candidate collides with
    // its neighbours, so the greedy pass has to space the seven it prints.
    const nodes: MemoryGraphNodeV1[] = [];
    const edges: MemoryGraphPayloadV1['edges'] = [];
    nodes.push({ id: 'entity:hub', kind: 'entity', entity_id: 'hub', label: 'hub' });
    for (let index = 0; index < 40; index += 1) {
      nodes.push(factNode(`f${index.toString().padStart(2, '0')}`, { trust_score: 0.9 }));
      edges.push({ kind: 'mentions', source: `fact:f${index.toString().padStart(2, '0')}`, target: 'entity:hub' });
    }
    const model = composeConstellation(graph(nodes, edges));
    const labelled = model.nodes.filter((node) => node.labelled);
    expect(labelled.length).toBeGreaterThan(0);
    expect(labelled.length).toBeLessThanOrEqual(7);
    for (const a of labelled) {
      for (const b of labelled) {
        if (a === b) continue;
        const sameRow = Math.abs(a.y - b.y) < 13;
        const sameSideOverlap = a.labelSide === b.labelSide && Math.abs(a.x - b.x) < 200;
        expect(sameRow && sameSideOverlap).toBe(false);
      }
    }
    // Determinism holds for the label pass as well as the coordinates.
    expect(composeConstellation(graph(nodes, edges)).nodes.filter((node) => node.labelled)).toEqual(labelled);
  });

  it('counts every trust band, zeroes included, and carries the daemon coverage verbatim', () => {
    const model = composeConstellation(
      graph(
        [
          factNode('a', { trust_score: 0.99 }),
          factNode('b', { trust_score: 0.8 }),
          factNode('c', { trust_score: 0.61 }),
          factNode('d', { trust_score: null }),
        ],
        [],
        {
          coverage: {
            completeness: 'unknown',
            eligible: null,
            examined: null,
            matched: null,
            excluded: null,
            omitted: null,
            unknown: null,
            denominator: null,
            unit: null,
            omission_reasons: ['fact_universe_bounded'],
          },
          fact_universe_count: 4128,
          fact_candidates_examined: 4,
          unavailable_fact_candidates: 2,
        },
      ),
    );
    expect(model.bands.map((band) => [band.id, band.count])).toEqual([
      ['b80', 2],
      ['b60', 1],
      ['b40', 0],
      ['b20', 0],
      ['b00', 0],
      ['unmeasured', 1],
    ]);
    expect(model.coverage.completeness).toBe('unknown');
    expect(model.coverage.omissionReasons).toEqual(['fact_universe_bounded']);
    expect(model.coverage.factUniverse).toBe(4128);
    expect(model.coverage.unavailableFactCandidates).toBe(2);
  });

  it('describes the measurements, the slice, and the exact fallback', () => {
    const model = composeConstellation(
      graph(
        [
          factNode('a', { trust_score: 0.9, category: 'decision' }),
          { id: 'entity:x', kind: 'entity', entity_id: 'x', label: 'X' },
        ],
        [{ kind: 'mentions', source: 'fact:a', target: 'entity:x' }],
        { fact_universe_count: 900, fact_candidates_examined: 1 },
      ),
    );
    const description = constellationDescription(model);
    expect(description).toContain('1 fact root');
    expect(description).toContain('decision 1');
    expect(description).toContain('1 at 0.80–1.00');
    expect(description).toContain('1 relation drawn (mentions)');
    expect(description).toContain('1 examined of 900 facts');
    expect(description).toContain('fact ledger beside this field is the exact accessible equivalent');
  });

  it('returns an empty model for an empty graph without inventing a sector', () => {
    const model = composeConstellation(graph([]));
    expect(model.nodes).toEqual([]);
    expect(model.sectors).toEqual([]);
    expect(model.links).toEqual([]);
    expect(model.bands.every((band) => band.count === 0)).toBe(true);
  });
});

describe('trust bands and glyphs', () => {
  it('bands trust at the printed edges', () => {
    expect(trustBandOf(1)).toBe('b80');
    expect(trustBandOf(0.8)).toBe('b80');
    expect(trustBandOf(0.79)).toBe('b60');
    expect(trustBandOf(0.4)).toBe('b40');
    expect(trustBandOf(0.2)).toBe('b20');
    expect(trustBandOf(0)).toBe('b00');
    expect(trustBandOf(null)).toBe('unmeasured');
    expect(trustBandOf(Number.NaN)).toBe('unmeasured');
  });

  it('dashes contradiction and supersession apart from a solid support line, so kind survives without colour', () => {
    expect(relationStyle('contradicts')).toEqual({ label: 'contradicts', dash: '5 4', tone: 'conflict' });
    expect(relationStyle('supersedes')).toEqual({
      label: 'supersedes',
      dash: '7 3 1.5 3',
      tone: 'stale',
    });
    expect(relationStyle('supports')).toEqual({ label: 'supports', dash: undefined, tone: 'signal' });
  });

  it('draws facts as discs and every satellite kind as a distinct glyph', () => {
    expect(nodeGlyph('fact').shape).toBe('disc');
    const satelliteShapes = new Set(
      (['entity', 'assertion', 'retrieval_anchor'] as const).map((kind) => nodeGlyph(kind).shape),
    );
    expect(satelliteShapes.size).toBe(3);
    expect(satelliteShapes.has('disc')).toBe(false);
  });
});
