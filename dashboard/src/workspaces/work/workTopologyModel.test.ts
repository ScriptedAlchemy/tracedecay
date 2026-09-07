import { describe, expect, it } from 'vitest';
import {
  ExecutionTopologyViewV1Schema,
  type ExecutionTopologyViewV1,
} from '../../contracts/index.ts';
import { workTopologyReading } from './workTopologyModel.ts';

function topology(generation = 'generation-7'): ExecutionTopologyViewV1 {
  return ExecutionTopologyViewV1Schema.parse({
    state: 'view',
    topology: { generation, task_count: 2 },
    coverage: { coverage: 'complete', returned: 2 },
    execution_placement: {
      mode: { kind: 'existing_worktree_only' },
      lanes: [
        {
          task_id: 'alpha',
          run_id: 'run-1',
          attempt_count: 2,
          placement: {
            state: 'placed',
            placement: {
              authority_version: 1,
              blockers: [],
              identity: { task_id: 'alpha', run_id: 'run-1' },
              retention_eligible_at: null,
              state: 'admitted',
              target: {
                kind: 'linked_worktree',
                in_place_acknowledged: false,
                network_free: true,
                root: '/work/alpha',
              },
              transitioned_at: 1,
            },
          },
        },
      ],
    },
    branch_topology: { allowed: ['unbranched'] },
    review_topology: { allowed: ['no_review'], github_stacked_prs: 'disabled' },
    integration_strategy: {
      cross_merge: { allow_cross_repository: false, allowed_modes: ['disabled'], default_mode: 'disabled' },
      gates: {
        cleanliness: 'require_clean',
        maximum_preflight_age_seconds: 300,
        require_fresh_preflight: true,
        review: { kind: 'none' },
        tests: [],
      },
      protected_refs: [],
    },
  });
}

describe('the canonical execution-topology reading', () => {
  it('keeps an unissued topology read loading rather than inventing empty dimensions', () => {
    const reading = workTopologyReading(undefined);
    for (const channel of [
      reading.binding,
      reading.coverage,
      reading.executionPlacement,
      reading.branchTopology,
      reading.reviewTopology,
      reading.integrationStrategy,
    ]) {
      expect(channel.available).toBe(false);
      if (channel.available) throw new Error('unreachable');
      expect(channel.state).toBe('loading');
    }
  });

  it('carries a route refusal to every canonical topology dimension', () => {
    const reading = workTopologyReading({
      outcome: 'refused',
      state: 'unavailable',
      detail: 'the Work runtime is unavailable',
    });
    expect(reading.executionPlacement).toMatchObject({ available: false, state: 'unavailable' });
    expect(reading.branchTopology).toMatchObject({ available: false, state: 'unavailable' });
  });

  it('renders all four dimensions from one generated topology payload without schema-gap stand-ins', () => {
    const reading = workTopologyReading({ outcome: 'value', value: topology() });
    for (const channel of [
      reading.binding,
      reading.coverage,
      reading.executionPlacement,
      reading.branchTopology,
      reading.reviewTopology,
      reading.integrationStrategy,
    ]) {
      expect(channel.available).toBe(true);
    }
    if (!reading.binding.available || !reading.executionPlacement.available) {
      throw new Error('expected canonical topology');
    }
    expect(reading.binding.value.generation).toBe('generation-7');
    expect(reading.executionPlacement.value.lanes[0]).toMatchObject({
      task_id: 'alpha',
      run_id: 'run-1',
      attempt_count: 2,
    });
  });

  it('keeps the canonical typed absence distinct from an empty lane page', () => {
    const reading = workTopologyReading({ outcome: 'value', value: { state: 'absent' } });
    expect(reading.executionPlacement).toMatchObject({ available: false, state: 'denied' });
  });
});
