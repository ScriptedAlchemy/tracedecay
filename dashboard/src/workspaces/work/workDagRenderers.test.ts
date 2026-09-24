import { describe, expect, it } from 'vitest';
import { workTaskView } from '../../test/workTaskViewFixture.ts';
import { workDagLayout } from './workDagLayout.ts';
import { dsmStep, workDsm } from './workDsmModel.ts';
import { laneTreatment } from './workLaneTreatment.ts';
import type { WorkTaskView } from './workProductView.ts';
import { NO_HANDOFF_LANE, swimlaneWidth, workSwimlaneLayout } from './workSwimlaneLayout.ts';
import { workDagReading } from './workViewsModel.ts';

/**
 * The swimlane and matrix renderers re-read one layered layout. These tests
 * pin what each derives from the same five-task plan: which lane a task sits
 * in and where its plate lands, which relations become matrix cells and
 * which of those are back-edges, and the typed-state family each lane wears.
 */

const handoff = (toActor: string, handedOffAt: number) => ({
  handoffId: `handoff.${toActor}.${handedOffAt}`,
  fromActor: 'actor.owner',
  toActor,
  handedOffAt,
});

const PLAN: readonly WorkTaskView[] = [
  workTaskView({ task_id: 'schema', hierarchy: { initiative_id: 'i', plan_id: 'p', milestone_id: 'contracts' } }),
  workTaskView({
    task_id: 'regen',
    dependencies: ['schema'],
    hierarchy: { initiative_id: 'i', plan_id: 'p', milestone_id: 'contracts' },
    handoffs: [handoff('actor.agents', 10), handoff('actor.review', 20)],
  }),
  workTaskView({
    task_id: 'rings',
    dependencies: ['regen'],
    hierarchy: { initiative_id: 'i', plan_id: 'p', milestone_id: 'dashboard' },
    handoffs: [handoff('actor.agents', 5)],
  }),
  workTaskView({
    task_id: 'cycle-a',
    dependencies: ['cycle-b', 'schema'],
    hierarchy: { initiative_id: 'i', plan_id: 'p', milestone_id: 'dashboard' },
  }),
  workTaskView({
    task_id: 'cycle-b',
    dependencies: ['cycle-a'],
    causal_candidates: ['rings'],
    hierarchy: { initiative_id: 'i', plan_id: 'p', milestone_id: 'dashboard' },
  }),
];

function read() {
  const reading = workDagReading(PLAN);
  const layout = workDagLayout(reading, PLAN);
  const tasks = new Map(PLAN.map((task) => [task.task_id, task]));
  return { reading, layout, tasks };
}

describe('workSwimlaneLayout', () => {
  it('lays milestones on y and stratum depth on x', () => {
    const { reading, layout, tasks } = read();
    const lanes = workSwimlaneLayout(layout, reading, tasks, 'milestone');
    expect(lanes.lanes.map((lane) => [lane.key, lane.grade, lane.taskIds])).toEqual([
      ['contracts', 'exact', ['schema', 'regen']],
      ['dashboard', 'exact', ['cycle-a', 'cycle-b', 'rings']],
    ]);
    expect(lanes.plates.map((plate) => [plate.taskId, plate.depth, plate.x, plate.y])).toEqual([
      ['schema', 0, 176, 38],
      ['regen', 1, 400, 38],
      ['cycle-a', 1, 400, 102],
      ['cycle-b', 1, 400, 154],
      ['rings', 2, 624, 102],
    ]);
    expect(lanes.width).toBe(swimlaneWidth(layout));
    expect(lanes.width).toBe(848);
  });

  it('routes forward edges through the gutter and arcs a cycle climb', () => {
    const { reading, layout, tasks } = read();
    const lanes = workSwimlaneLayout(layout, reading, tasks, 'milestone');
    const path = (id: string) => lanes.edges.find((edge) => edge.id === id)?.path;
    expect(path('gating:schema->regen')).toBe('M 360 60 H 388 V 60 H 400');
    expect(path('gating:regen->rings')).toBe('M 584 60 H 612 V 124 H 624');
    const climbs = lanes.edges.filter((edge) => edge.climb).map((edge) => edge.id);
    expect(climbs).toEqual(['gating:cycle-a->cycle-b', 'gating:cycle-b->cycle-a']);
    expect(path('gating:cycle-b->cycle-a')).toMatch(/^M 492 154 C /);
  });

  it('emphasises the deepest declared chain, not an effort weighting', () => {
    const { reading, layout, tasks } = read();
    const lanes = workSwimlaneLayout(layout, reading, tasks, 'milestone');
    expect(lanes.chain.depth).toBe(3);
    expect([...lanes.chain.tasks].sort()).toEqual(['regen', 'rings', 'schema']);
    expect([...lanes.chain.edges].sort()).toEqual(['gating:regen->rings', 'gating:schema->regen']);
  });

  it('lanes by the latest recorded holder and names the tasks nobody handed off', () => {
    const { reading, layout, tasks } = read();
    const lanes = workSwimlaneLayout(layout, reading, tasks, 'holder');
    expect(lanes.lanes.map((lane) => [lane.key, lane.grade, lane.taskIds])).toEqual([
      ['actor.review', 'explicit', ['regen']],
      ['actor.agents', 'explicit', ['rings']],
      [NO_HANDOFF_LANE, 'unavailable', ['schema', 'cycle-a', 'cycle-b']],
    ]);
  });
});

describe('workDsm', () => {
  it('orders both axes by the layered reading and marks back-edges only for gating', () => {
    const { layout } = read();
    const dsm = workDsm(layout);
    expect(dsm.order).toEqual(['schema', 'cycle-a', 'cycle-b', 'regen', 'rings']);
    expect(dsm.cells.map((cell) => [cell.id, cell.row, cell.column, cell.back])).toEqual([
      ['gating:schema->cycle-a', 1, 0, false],
      ['gating:cycle-b->cycle-a', 1, 2, true],
      ['gating:cycle-a->cycle-b', 2, 1, true],
      ['causal:rings->cycle-b', 2, 4, false],
      ['gating:schema->regen', 3, 0, false],
      ['gating:regen->rings', 4, 3, false],
    ]);
    expect(dsm.backEdges).toBe(2);
    expect(dsm.blocks).toEqual([
      { depth: 0, start: 0, end: 0 },
      { depth: 1, start: 1, end: 3 },
      { depth: 2, start: 4, end: 4 },
    ]);
  });

  it('steps the active row by listbox keys and ignores every other key', () => {
    expect(dsmStep('ArrowDown', null, 5)).toBe(0);
    expect(dsmStep('ArrowDown', 4, 5)).toBe(4);
    expect(dsmStep('ArrowUp', 0, 5)).toBe(0);
    expect(dsmStep('End', 1, 5)).toBe(4);
    expect(dsmStep('PageDown', 0, 30)).toBe(10);
    expect(dsmStep('a', 2, 5)).toBeNull();
    expect(dsmStep('Home', null, 0)).toBeNull();
  });
});

describe('laneTreatment', () => {
  it('maps every lane to the design system typed-state family', () => {
    const family = (lane: Parameters<typeof laneTreatment>[0]) => {
      const treatment = laneTreatment(lane);
      return [treatment.family, treatment.hatched, treatment.dashed];
    };
    expect(family({ kind: 'projected', lane: 'blocked' })).toEqual(['degraded', true, false]);
    expect(family({ kind: 'projected', lane: 'review' })).toEqual(['degraded', true, false]);
    expect(family({ kind: 'projected', lane: 'cancelled' })).toEqual(['disconnected', false, true]);
    expect(family({ kind: 'projected', lane: 'unavailable' })).toEqual(['disconnected', false, true]);
    expect(family({ kind: 'projected', lane: 'running' })).toEqual(['activity', false, false]);
    expect(family({ kind: 'projected', lane: 'done' })).toEqual(['ready', false, false]);
    expect(family({ kind: 'projected', lane: 'scheduled' })).toEqual(['loading', false, false]);
    expect(family({ kind: 'projected', lane: 'todo' })).toEqual(['neutral', false, false]);
    expect(family({ kind: 'uncarded' })).toEqual(['disconnected', false, true]);
  });
});
