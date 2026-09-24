import { describe, expect, it } from 'vitest';
import { workTaskView } from '../../test/workTaskViewFixture.ts';
import { workDagLayout } from './workDagLayout.ts';
import { dsmStep, workDsm } from './workDsmModel.ts';
import { laneTreatment } from './workLaneTreatment.ts';
import type { WorkTaskView } from './workProductView.ts';
import { workDagReading } from './workViewsModel.ts';

/**
 * The matrix re-reads the layered layout. These tests pin what it derives
 * from a five-task plan with a declared cycle: which relations become cells,
 * which of those are back-edges, how bright each cell is, and the
 * typed-state family each lane wears.
 */

const PLAN: readonly WorkTaskView[] = [
  workTaskView({ task_id: 'schema', hierarchy: { initiative_id: 'i', plan_id: 'p', milestone_id: 'contracts' } }),
  workTaskView({
    task_id: 'regen',
    dependencies: ['schema'],
    hierarchy: { initiative_id: 'i', plan_id: 'p', milestone_id: 'contracts' },
  }),
  workTaskView({
    task_id: 'rings',
    dependencies: ['regen'],
    hierarchy: { initiative_id: 'i', plan_id: 'p', milestone_id: 'dashboard' },
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
  return { reading, layout };
}

describe('workDsm', () => {
  it('orders both axes by the layered reading and marks back-edges only for gating', () => {
    const { layout } = read();
    const dsm = workDsm(layout);
    expect(dsm.order).toEqual(['schema', 'cycle-a', 'cycle-b', 'regen', 'rings']);
    expect(dsm.cells.map((cell) => [cell.id, cell.row, cell.column, cell.back, cell.intensity])).toEqual([
      ['gating:schema->cycle-a', 1, 0, false, 0.95],
      ['gating:cycle-b->cycle-a', 1, 2, true, 0.65],
      ['gating:cycle-a->cycle-b', 2, 1, true, 0.65],
      ['causal:rings->cycle-b', 2, 4, false, 0.65],
      ['gating:schema->regen', 3, 0, false, 0.95],
      ['gating:regen->rings', 4, 3, false, 0.65],
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
