import { describe, expect, it } from 'vitest';

import type { MemoryFactRowV1, MemoryGraphPayloadV1 } from '../../../contracts/generated.ts';
import { layoutCameras } from './cameras.ts';
import { composeFactScene, elideToWidth, hubFact } from './factScene.ts';
import { NO_ENTITY_HUB, layoutLattice } from './lattice.ts';
import { layoutTrustField } from './trustField.ts';
import { parseConstellationVariant } from './variant.ts';

const DAY = 86_400_000_000;
/** 2026-09-01T00:00:00Z in microseconds. */
const T0 = Date.UTC(2026, 8, 1) * 1000;
const WITHHELD = 'fact.withheld.000042';

function factNode(id: string, trust: number | null, category: string | null, retrievals: number | null, helpful: number | null) {
  return {
    id: `fact:${id}`,
    kind: 'fact' as const,
    label: `${id} content`,
    fact_id: id,
    payload_access: 'eligible' as const,
    projected_as_of: T0,
    content: `${id} content`,
    category,
    trust_score: trust,
    retrieval_count: retrievals,
    helpful_count: helpful,
  };
}

function graph(): MemoryGraphPayloadV1 {
  return {
    nodes: [
      factNode('fact-one', 0.9, 'decision', 40, 3),
      factNode('fact-two', 0.4, 'decision', 10, 0),
      factNode('fact-three', 0.7, 'tool', 20, 2),
      {
        id: `fact:${WITHHELD}`,
        kind: 'fact',
        label: WITHHELD,
        fact_id: WITHHELD,
        payload_access: 'redacted',
        projected_as_of: T0,
        content: null,
        category: null,
        trust_score: null,
        retrieval_count: null,
        helpful_count: null,
      },
      { id: 'entity:A', kind: 'entity', entity_id: 'A', label: 'A' },
      { id: 'entity:B', kind: 'entity', entity_id: 'B', label: 'B' },
      { id: 'assertion:x', kind: 'assertion', assertion_id: 'x', fact_id: 'fact-one', label: 'x' },
    ],
    edges: [
      { kind: 'mentions', source: 'fact:fact-one', target: 'entity:A' },
      { kind: 'mentions', source: 'fact:fact-two', target: 'entity:A' },
      { kind: 'mentions', source: 'fact:fact-two', target: 'entity:B' },
      { kind: 'mentions', source: 'fact:fact-three', target: 'entity:B' },
      { kind: 'supports', source: 'fact:fact-one', target: 'fact:fact-two' },
      { kind: 'contradicts', source: 'fact:fact-one', target: 'fact:fact-three' },
      { kind: 'supersedes', source: 'fact:fact-three', target: `fact:${WITHHELD}` },
      { kind: 'active_assertion', source: 'fact:fact-one', target: 'assertion:x' },
    ],
    coverage: {
      completeness: 'partial',
      eligible: null,
      examined: null,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: null,
      unit: null,
      omission_reasons: [],
    },
    fact_universe_count: 40,
    fact_candidates_examined: 4,
    unavailable_fact_candidates: 0,
    root_count: 4,
    relation_limit: 100,
    relation_count: 8,
  };
}

function row(factId: string, updatedAt: number | null): MemoryFactRowV1 {
  return {
    fact_id: factId,
    payload_access: 'eligible',
    trust_score: null,
    retrieval_count: null,
    access_count: null,
    helpful_count: null,
    unhelpful_count: null,
    created_at: null,
    updated_at: updatedAt,
    last_recalled_at: null,
    projected_as_of: T0,
    content: null,
    category: null,
    tags: null,
    entities: null,
    linked_entities: null,
    metadata: null,
    source_label: null,
  };
}

const ROWS = [row('fact-one', T0 + 2 * DAY), row('fact-two', T0), row('fact-three', null)];

describe('composeFactScene', () => {
  const scene = composeFactScene(graph(), ROWS);

  it('joins graph facts to their loaded rows and prints a withheld fact by identity, never content', () => {
    expect(scene.facts.map((fact) => [fact.factId, fact.updatedAt, fact.rowLoaded])).toEqual([
      ['fact-one', T0 + 2 * DAY, true],
      ['fact-two', T0, true],
      ['fact-three', null, true],
      [WITHHELD, null, false],
    ]);
    const withheld = scene.byNode.get(`fact:${WITHHELD}`)!;
    expect(withheld.label).toBe('redacted · …000042');
    expect(withheld.restricted).toBe(true);
    expect(withheld.band).toBe('unmeasured');
  });

  it('groups facts under the entities they cite by name and keeps only fact-to-fact relations', () => {
    expect(scene.byNode.get('fact:fact-two')!.entityIds).toEqual(['entity:A', 'entity:B']);
    expect(scene.entities.map((entity) => [entity.label, entity.factIds])).toEqual([
      ['A', ['fact:fact-one', 'fact:fact-two']],
      ['B', ['fact:fact-three', 'fact:fact-two']],
    ]);
    expect(scene.relations.map((relation) => relation.kind)).toEqual(['supports', 'contradicts', 'supersedes']);
    expect(scene.unplaced).toBe(1);
    expect(scene.unplacedRelations).toBe(1);
  });

  it('names the most wired readable fact as the hub', () => {
    expect(hubFact(scene)?.factId).toBe('fact-one');
    expect(scene.byNode.get('fact:fact-one')!.degree).toBe(4);
  });

  it('elides to a pixel budget at the printed mono advance', () => {
    expect(elideToWidth('abcdefghijklmnop', 66, 11)).toBe('abcdefghi…');
    expect(elideToWidth('short', 66, 11)).toBe('short');
  });
});

describe('layoutCameras', () => {
  const scene = composeFactScene(graph(), ROWS);
  const layout = layoutCameras(scene, 960);

  it('frames each category, largest first and the absent category last, on one shared rail', () => {
    // Frames in one band share its height so their rows start level.
    expect(layout.frames.map((frame) => [frame.title, frame.count, frame.x, frame.w, frame.h])).toEqual([
      ['decision', 2, 12, 300, 60],
      ['tool', 1, 330, 300, 60],
      ['category absent', 1, 648, 300, 60],
    ]);
    expect(layout.rail).toEqual({ x: 194, w: 60 });
    expect(layout.labelW).toBe(160);
    expect(layout.height).toBe(84);
    expect(layout.frames[0]!.rows.map((entry) => [entry.fact.factId, entry.gx, entry.gy])).toEqual([
      ['fact-one', 24, 42],
      ['fact-two', 24, 58],
    ]);
    expect(layout.frames[0]!.cites).toEqual([
      { label: 'A', count: 2 },
      { label: 'B', count: 1 },
    ]);
  });

  it('routes relations through the glyph gutter and the gaps between frames', () => {
    expect(layout.relations.map((relation) => [relation.relation.kind, relation.d, relation.vertical])).toEqual([
      ['supports', 'M 24 42 C 7.2 42, 7.2 58, 24 58', true],
      ['contradicts', 'M 24 42 H 3 V 3 H 321 V 42 H 342', false],
      ['supersedes', 'M 342 42 H 323.5 V 5.5 H 641.5 V 42 H 660', false],
    ]);
    expect(layout.relationsPastCap).toBe(0);
  });

  it('caps a frame at eight rows, names the rest, and counts relations it cannot reach', () => {
    const many = graph();
    many.nodes = Array.from({ length: 10 }, (_, index) => factNode(`f${index}`, index / 10, 'bulk', 1, 1));
    many.edges = [{ kind: 'supports', source: 'fact:f9', target: 'fact:f0' }];
    const capped = layoutCameras(composeFactScene(many, []), 960);
    expect(capped.frames[0]!.rows).toHaveLength(8);
    expect(capped.frames[0]!.overflow).toBe(2);
    expect(capped.relations).toHaveLength(0);
    expect(capped.relationsPastCap).toBe(1);
  });
});

describe('layoutTrustField', () => {
  const scene = composeFactScene(graph(), ROWS);

  it('places trust on one linear scale and last update newest-up, with absent readings in printed gutters', () => {
    const layout = layoutTrustField(scene, { width: 960, height: 360 }, null);
    expect(layout.plot).toEqual({ x0: 52, x1: 902, y0: 14, y1: 318 });
    expect(layout.points.map((point) => [point.fact.factId, point.x, point.y, point.r])).toEqual([
      ['fact-one', 817, 30.29, 9],
      ['fact-two', 392, 301.71, 6],
      ['fact-three', 647, 334, 7.24],
      [WITHHELD, 924, 334, 3],
    ]);
    expect(layout.trustAbsent).toBe(1);
    expect(layout.timeAbsent).toBe(2);
    expect(layout.xTicks.map((tick) => [tick.label, tick.at])).toEqual([
      ['0.00', 52],
      ['0.20', 222],
      ['0.40', 392],
      ['0.60', 562],
      ['0.80', 732],
      ['1.00', 902],
    ]);
    expect(layout.yTicks.map((tick) => tick.label)).toEqual(['Sep 1', 'Sep 2', 'Sep 3']);
    expect(layout.relations.map((relation) => [relation.relation.kind, relation.loud])).toEqual([
      ['supports', false],
      ['contradicts', true],
      ['supersedes', true],
    ]);
    expect(layout.points.filter((point) => point.labelled).map((point) => point.fact.factId)).toEqual([
      'fact-one',
      'fact-two',
      'fact-three',
    ]);
    expect(layout.envelopes.map((envelope) => envelope.label)).toEqual(['A', 'B']);
  });

  it('zooms into an entity by re-deriving both domains from its members', () => {
    const layout = layoutTrustField(scene, { width: 960, height: 360 }, 'entity:B');
    expect(layout.zoom).toEqual({ entityId: 'entity:B', label: 'B', count: 2 });
    expect(layout.trustDomain).toEqual([0.37, 0.73]);
    const at = new Map(layout.points.map((point) => [point.fact.factId, point]));
    expect([at.get('fact-two')!.x, at.get('fact-two')!.y]).toEqual([122.83, 166]);
    expect(at.get('fact-three')!.x).toBe(831.17);
    expect(at.get('fact-one')!.visible).toBe(false);
    expect(at.get('fact-one')!.inZoom).toBe(false);
    expect(layout.envelopes.map((envelope) => envelope.entityId)).toEqual(['entity:B']);
  });

  it('states an unmeasured vertical axis rather than inventing one', () => {
    const layout = layoutTrustField(scene, { width: 960, height: 360 }, null);
    const bare = layoutTrustField(composeFactScene(graph(), []), { width: 960, height: 360 }, null);
    expect(layout.timeDomain).not.toBeNull();
    expect(bare.timeDomain).toBeNull();
    expect(bare.yTicks).toEqual([]);
    expect(bare.timeAbsent).toBe(4);
  });
});

describe('layoutLattice', () => {
  const scene = composeFactScene(graph(), ROWS);

  it('orbits each fact on the first entity it cites, with a hub for facts citing none', () => {
    const layout = layoutLattice(scene, { width: 960, height: 360 });
    expect(layout.hubs.map((hub) => [hub.id, hub.count])).toEqual([
      ['entity:A', 2],
      ['entity:B', 1],
      [NO_ENTITY_HUB, 1],
    ]);
    expect(layout.satellites.map((satellite) => [satellite.fact.factId, satellite.hubId, satellite.unconfirmed])).toEqual([
      ['fact-one', 'entity:A', false],
      ['fact-two', 'entity:A', true],
      ['fact-three', 'entity:B', false],
      [WITHHELD, NO_ENTITY_HUB, false],
    ]);
    expect(layout.spokes.map((spoke) => [spoke.factNodeId, spoke.hubId, spoke.primary])).toEqual([
      ['fact:fact-one', 'entity:A', true],
      ['fact:fact-two', 'entity:A', true],
      ['fact:fact-two', 'entity:B', false],
      ['fact:fact-three', 'entity:B', true],
    ]);
  });

  it('bundles cross-hub relations per hub pair and kind, and chords relations inside one hub', () => {
    const layout = layoutLattice(scene, { width: 960, height: 360 });
    expect(layout.bundles.map((bundle) => [bundle.key, bundle.count])).toEqual([
      ['entity:A|entity:B|contradicts', 1],
      [`entity:B|${NO_ENTITY_HUB}|supersedes`, 1],
    ]);
    expect(layout.chords.map((chord) => chord.relation.kind)).toEqual(['supports']);
  });

  it('settles to the same picture every time and keeps orbits apart', () => {
    const a = layoutLattice(scene, { width: 960, height: 360 });
    const b = layoutLattice(scene, { width: 960, height: 360 });
    expect(b).toEqual(a);
    for (const [i, one] of a.hubs.entries()) {
      for (const other of a.hubs.slice(i + 1)) {
        expect(Math.hypot(one.x - other.x, one.y - other.y)).toBeGreaterThanOrEqual(one.orbit + other.orbit + 17.9);
      }
    }
  });

  it('folds satellites past an orbit capacity into one aggregate with the exact count and trust range', () => {
    const dense = graph();
    dense.nodes = [
      ...Array.from({ length: 30 }, (_, index) => factNode(`d${index}`, (index + 1) / 100, 'bulk', 1, 1)),
      { id: 'entity:Hub', kind: 'entity', entity_id: 'Hub', label: 'Hub' },
    ];
    dense.edges = [
      ...Array.from({ length: 30 }, (_, index) => ({ kind: 'mentions' as const, source: `fact:d${index}`, target: 'entity:Hub' })),
      { kind: 'supports', source: 'fact:d0', target: 'fact:d29' },
    ];
    const layout = layoutLattice(composeFactScene(dense, []), { width: 960, height: 360 });
    expect(layout.hubs[0]!.orbit).toBe(52.35);
    expect(layout.satellites).toHaveLength(19);
    expect(layout.aggregates).toHaveLength(1);
    expect(layout.aggregates[0]!.count).toBe(11);
    expect(layout.aggregates[0]!.trustRange![0]).toBeCloseTo(0.01);
    expect(layout.aggregates[0]!.trustRange![1]).toBeCloseTo(0.11);
    expect(layout.relationsIntoAggregates).toBe(1);
  });
});

describe('parseConstellationVariant', () => {
  it('accepts the three candidates and nothing else', () => {
    expect(['cameras', 'field', 'lattice', 'polar', null].map((value) => parseConstellationVariant(value))).toEqual([
      'cameras',
      'field',
      'lattice',
      null,
      null,
    ]);
  });
});
