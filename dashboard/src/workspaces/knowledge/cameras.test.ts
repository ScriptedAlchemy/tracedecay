import { describe, expect, it } from 'vitest';

import type { MemoryFactRowV1, MemoryGraphPayloadV1 } from '../../contracts/generated.ts';
import { layoutCameras } from './cameras.ts';
import {
  composeFactScene,
  disputesOf,
  elideToWidth,
  hubFact,
  relationStyle,
  trustBandOf,
} from './factScene.ts';

const DAY = 86_400_000_000;
/** 2026-09-01T00:00:00Z in microseconds. */
const T0 = Date.UTC(2026, 8, 1) * 1000;
const WITHHELD = 'fact.withheld.000042';

function factNode(id: string, trust: number | null, category: string | null, retrievals: number | null = 1) {
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
    helpful_count: 1,
  };
}

function graph(): MemoryGraphPayloadV1 {
  return {
    nodes: [
      factNode('fact-one', 0.9, 'decision', 40),
      factNode('fact-two', 0.4, 'decision', 10),
      factNode('fact-three', 0.7, 'tool', 20),
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
      { kind: 'supports', source: 'fact:fact-one', target: 'fact:not-in-this-slice' },
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
      omission_reasons: ['fact_universe_bounded'],
    },
    fact_universe_count: 40,
    fact_candidates_examined: 4,
    unavailable_fact_candidates: 1,
    root_count: 4,
    relation_limit: 100,
    relation_count: 9,
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

function bulk(count: number): MemoryGraphPayloadV1 {
  return {
    ...graph(),
    nodes: Array.from({ length: count }, (_, index) => factNode(`b${index}`, index / 10, 'bulk')),
    edges: [],
  };
}

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
    expect(disputesOf(scene).map((relation) => relation.kind)).toEqual(['contradicts', 'supersedes']);
    expect(scene.unplaced).toBe(1);
    expect(scene.unplacedRelations).toBe(1);
  });

  it("counts a relation to a node the slice did not include as dangling and keeps the daemon's accounting", () => {
    expect(scene.coverage).toEqual({
      drawnFacts: 4,
      drawnRelations: 8,
      danglingRelations: 1,
      factUniverse: 40,
      factCandidatesExamined: 4,
      unavailableFactCandidates: 1,
      withheldDrawn: 1,
      relationCount: 9,
      relationLimit: 100,
      completeness: 'partial',
      omissionReasons: ['fact_universe_bounded'],
    });
    expect(scene.byNode.get('fact:fact-one')!.degree).toBe(4);
    expect([...scene.neighbours.get('fact:fact-one')!].sort()).toEqual([
      'assertion:x',
      'entity:A',
      'fact:fact-three',
      'fact:fact-two',
    ]);
  });

  it('names the most wired readable fact as the hub', () => {
    expect(hubFact(scene)?.factId).toBe('fact-one');
  });

  it('bands trust inclusively at the lower edge and names relation kinds by line style', () => {
    expect([1, 0.8, 0.79, 0.4, 0.2, 0, null, Number.NaN].map((trust) => trustBandOf(trust))).toEqual([
      'b80',
      'b80',
      'b60',
      'b40',
      'b20',
      'b00',
      'unmeasured',
      'unmeasured',
    ]);
    expect(relationStyle('contradicts')).toEqual({ label: 'contradicts', dash: '5 4', tone: 'conflict' });
    expect(relationStyle('supersedes')).toEqual({ label: 'supersedes', dash: '7 3 1.5 3', tone: 'stale' });
    expect(relationStyle('supports')).toEqual({ label: 'supports', dash: undefined, tone: 'signal' });
  });

  it('elides to a pixel budget at a face advance', () => {
    expect(elideToWidth('abcdefghijklmnop', 66, 11)).toBe('abcdefghi…');
    expect(elideToWidth('short', 66, 11)).toBe('short');
    expect(elideToWidth('abcdefghijklmnop', 74.2, 14, 0.53)).toBe('abcdefghi…');
  });
});

describe('layoutCameras', () => {
  const scene = composeFactScene(graph(), ROWS);

  it('draws every row when the frames fit the box, on one shared rail', () => {
    const layout = layoutCameras(scene, { width: 960, height: 400 }, null);
    expect(layout.mode).toBe('rows');
    expect(layout.height).toBe(90);
    expect(layout.frames.map((frame) => [frame.title, frame.count, frame.x, frame.y, frame.w, frame.h])).toEqual([
      ['category absent', 1, 12, 12, 300, 66],
      ['decision', 2, 330, 12, 300, 66],
      ['tool', 1, 648, 12, 300, 66],
    ]);
    expect(layout.rail).toEqual({ x: 192, w: 60 });
    expect(layout.labelW).toBe(158);
    expect(layout.frames[1]!.rows.map((entry) => [entry.fact.factId, entry.gx, entry.gy])).toEqual([
      ['fact-one', 342, 44],
      ['fact-two', 342, 64],
    ]);
    expect(layout.frames[1]!.cites).toEqual([
      { label: 'A', count: 2 },
      { label: 'B', count: 1 },
    ]);
  });

  it('routes relations through the glyph gutter and the gaps between frames', () => {
    const layout = layoutCameras(scene, { width: 960, height: 400 }, null);
    expect(layout.relations.map((relation) => [relation.relation.kind, relation.d, relation.vertical])).toEqual([
      ['supports', 'M 342 44 C 325 44, 325 64, 342 64', true],
      ['contradicts', 'M 342 44 H 321 V 3 H 639 V 44 H 660', false],
      ['supersedes', 'M 660 44 H 641.5 V 5.5 H 5.5 V 44 H 24', false],
    ]);
    expect(layout.relationsOffField).toBe(0);
  });

  it('aggregates into frames with exact counts and one tick per fact when the rows do not fit', () => {
    const layout = layoutCameras(scene, { width: 960, height: 80 }, null);
    expect(layout.mode).toBe('aggregate');
    expect(layout.height).toBe(66);
    expect(
      layout.frames.map((frame) => [frame.title, frame.count, frame.x, frame.w, frame.h, frame.rail, frame.disputes]),
    ).toEqual([
      ['category absent', 1, 12, 300, 54, { x: 30, w: 272, y: 33 }, 1],
      ['decision', 2, 330, 300, 54, { x: 348, w: 272, y: 33 }, 1],
      ['tool', 1, 648, 300, 54, { x: 666, w: 272, y: 33 }, 2],
    ]);
    expect(layout.frames.map((frame) => frame.ticks.map((tick) => [tick.fact.factId, tick.x, tick.disputed]))).toEqual([
      [[WITHHELD, null, true]],
      [
        ['fact-one', 592.8, true],
        ['fact-two', 456.8, false],
      ],
      [['fact-three', 856.4, true]],
    ]);
    expect(layout.frames.map((frame) => [frame.trustRange, frame.trustAbsent, frame.withheld])).toEqual([
      [null, 1, 1],
      [[0.4, 0.9], 0, 0],
      [[0.7, 0.7], 0, 0],
    ]);
    expect(layout.frames.every((frame) => frame.rows.length === 0)).toBe(true);
    expect(layout.relations).toEqual([]);
    expect(layout.relationsOffField).toBe(3);
  });

  it('opens one frame across the field, in as many row columns as fit, counting what is off the field', () => {
    const tall = layoutCameras(scene, { width: 960, height: 100 }, { key: 'decision' });
    expect(tall.frames[0]!.rows.map((entry) => [entry.fact.factId, entry.x, entry.top])).toEqual([
      ['fact-one', 24, 28],
      ['fact-two', 24, 48],
    ]);
    expect(tall.relations.map((relation) => relation.d)).toEqual(['M 36 38 C 19 38, 19 58, 36 58']);
    // A relation with one drawn end keeps a stub at that end; one with
    // neither end drawn is only counted.
    expect(tall.stubs.map((stub) => [stub.relation.kind, stub.node, stub.other, stub.d])).toEqual([
      ['contradicts', 'fact:fact-one', { key: 'tool' }, 'M 36 38 H 26'],
    ]);
    expect(tall.relationsOffField).toBe(2);

    const layout = layoutCameras(scene, { width: 960, height: 60 }, { key: 'decision' });
    expect(layout.mode).toBe('open');
    expect(layout.height).toBe(58);
    expect(layout.frames.map((frame) => [frame.title, frame.x, frame.y, frame.w, frame.h, frame.overflow])).toEqual([
      ['decision', 12, 6, 936, 46, 0],
    ]);
    expect(layout.frames[0]!.rows.map((entry) => [entry.fact.factId, entry.x, entry.top, entry.w])).toEqual([
      ['fact-one', 24, 28, 292],
      ['fact-two', 334, 28, 292],
    ]);
    expect(layout.rail).toEqual({ x: 186, w: 58 });
    expect(layout.relations.map((relation) => relation.d)).toEqual(['M 36 38 H 12.5 V 52.5 H 322.5 V 38 H 346']);
    expect(layout.relationsOffField).toBe(2);
  });

  it('opens the category-absent frame by its null key', () => {
    const layout = layoutCameras(scene, { width: 960, height: 100 }, { key: null });
    expect(layout.frames.map((frame) => [frame.title, frame.rows.map((entry) => entry.fact.factId)])).toEqual([
      ['category absent', [WITHHELD]],
    ]);
  });

  it('caps a rows frame at eight and an opened frame at its capacity, naming the rest', () => {
    const rows = layoutCameras(composeFactScene(bulk(10), []), { width: 960, height: 1000 }, null);
    expect(rows.mode).toBe('rows');
    expect(rows.frames[0]!.rows).toHaveLength(8);
    expect([rows.frames[0]!.overflow, rows.frames[0]!.overflowAt]).toEqual([2, { x: 36, y: 208 }]);

    const opened = layoutCameras(composeFactScene(bulk(10), []), { width: 960, height: 100 }, { key: 'bulk' });
    expect(opened.frames[0]!.rows.map((entry) => entry.fact.factId)).toEqual(['b9', 'b8', 'b7', 'b6', 'b5', 'b4', 'b3', 'b2']);
    expect([opened.frames[0]!.overflow, opened.frames[0]!.overflowAt]).toEqual([2, { x: 668, y: 82 }]);
  });
});
