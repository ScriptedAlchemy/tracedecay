import { describe, expect, it } from 'vitest';
import type { WorkflowDefinition, WorkflowRunProjection } from '../../contracts/index.ts';
import {
  DEFINITION_ABSENCES,
  definitionShape,
  filterRegistry,
  formatDurationMicros,
  groupRegistry,
  recordReceipt,
  runStepSequence,
  runTiming,
  stepNeighbours,
  succeededStepCount,
  versionTrack,
} from './workflowLedger.ts';

const DIGEST_A = `sha256:${'a'.repeat(64)}`;
const DIGEST_B = `sha256:${'b'.repeat(64)}`;

function definition(overrides: Partial<WorkflowDefinition> = {}): WorkflowDefinition {
  return {
    definition_id: 'workflow.release-train',
    definition_version: 1,
    project_id: 'project.workflows',
    steps: [
      {
        step_id: 'fan-out',
        operation: 'operation.work.start_attempt',
        predecessors: [],
        inputs: [],
        outputs: ['finding'],
        fan_out: { max_width: 3 },
      },
      {
        step_id: 'collect',
        operation: 'operation.work.synthesize',
        predecessors: ['fan-out'],
        inputs: [{ producer_step_id: 'fan-out', output_name: 'finding' }],
        outputs: [],
        fan_out: null,
      },
    ],
    pinned_policy_digest: DIGEST_A,
    pinned_configuration_digest: DIGEST_A,
    pinned_catalog_digest: DIGEST_A,
    ...overrides,
  };
}

describe('groupRegistry', () => {
  it('groups every served version under its stable identity, ascending', () => {
    const entries = groupRegistry([
      definition({ definition_version: 3 }),
      definition({ definition_id: 'workflow.nightly', definition_version: 1 }),
      definition({ definition_version: 1 }),
      definition({ definition_version: 2 }),
    ]);
    expect(entries.map((entry) => entry.definitionId)).toEqual([
      'workflow.nightly',
      'workflow.release-train',
    ]);
    const train = entries[1]!;
    expect(train.versions.map((version) => version.definition_version)).toEqual([1, 2, 3]);
    expect(train.latest.definition_version).toBe(3);
    expect(train.projectAgreement).toBe('agree');
  });

  it('keeps a version the journal served twice as one immutable row', () => {
    const entries = groupRegistry([definition(), definition()]);
    expect(entries[0]!.versions).toHaveLength(1);
  });

  it('marks versions that disagree about their project as ambiguous rather than picking one', () => {
    const entries = groupRegistry([
      definition({ definition_version: 1, project_id: 'project.a' }),
      definition({ definition_version: 2, project_id: 'project.b' }),
    ]);
    expect(entries[0]!.projectAgreement).toBe('disagree');
  });

  it('filters on the identity only', () => {
    const entries = groupRegistry([
      definition(),
      definition({ definition_id: 'workflow.nightly-sweep' }),
    ]);
    expect(filterRegistry(entries, 'NIGHT').map((entry) => entry.definitionId)).toEqual([
      'workflow.nightly-sweep',
    ]);
    expect(filterRegistry(entries, '  ')).toHaveLength(2);
    // Digest text is never a match target.
    expect(filterRegistry(entries, 'aaaa')).toHaveLength(0);
  });
});

describe('definitionShape', () => {
  it('counts what the decoded step graph declares', () => {
    const shape = definitionShape(definition());
    expect(shape).toEqual({
      steps: 2,
      entrySteps: 1,
      terminalSteps: 1,
      fanOutSteps: 1,
      maxFanOutWidth: 3,
      declaredOutputs: 1,
      distinctOperations: 2,
      unresolvedReferences: [],
    });
  });

  it('reports references to steps the version does not declare', () => {
    const shape = definitionShape(
      definition({
        steps: [
          {
            step_id: 'collect',
            operation: 'operation.work.synthesize',
            predecessors: ['ghost'],
            inputs: [{ producer_step_id: 'phantom', output_name: 'x' }],
            outputs: [],
            fan_out: null,
          },
        ],
      }),
    );
    expect(shape.unresolvedReferences).toEqual(['ghost', 'phantom']);
    expect(shape.maxFanOutWidth).toBeNull();
  });

  it('names the plate fields the contract does not carry', () => {
    expect(DEFINITION_ABSENCES.map((absence) => absence.field)).toEqual([
      'created / updated',
      'description',
      'step timeout / retries',
    ]);
  });
});

describe('stepNeighbours', () => {
  it('lights one hop upstream and downstream from declared edges', () => {
    const steps = definition().steps;
    expect([...stepNeighbours(steps, 'fan-out').downstream]).toEqual(['collect']);
    expect([...stepNeighbours(steps, 'fan-out').upstream]).toEqual([]);
    expect([...stepNeighbours(steps, 'collect').upstream]).toEqual(['fan-out']);
  });
});

describe('versionTrack', () => {
  it('reads pin deltas and step-count deltas against the previous version', () => {
    const track = versionTrack([
      definition({ definition_version: 2, pinned_policy_digest: DIGEST_B }),
      definition({ definition_version: 1 }),
      definition({
        definition_version: 3,
        pinned_policy_digest: DIGEST_B,
        pinned_catalog_digest: DIGEST_B,
        steps: definition().steps.slice(0, 1),
      }),
    ]);
    expect(track.map((row) => row.definition.definition_version)).toEqual([1, 2, 3]);
    expect(track[0]).toMatchObject({
      policy: 'first',
      configuration: 'first',
      catalog: 'first',
      stepsDelta: null,
    });
    expect(track[1]).toMatchObject({
      policy: 'changed',
      configuration: 'same',
      catalog: 'same',
      stepsDelta: 0,
    });
    expect(track[2]).toMatchObject({
      policy: 'same',
      configuration: 'same',
      catalog: 'changed',
      stepsDelta: -1,
    });
  });
});

describe('recordReceipt', () => {
  it('keys the daemon\'s answer by the exact version it answered for', () => {
    const ledger = recordReceipt(new Map(), {
      action: 'activate',
      expectedRevision: 1,
      answeredAtMillis: 1_000,
      disposition: {
        definition_id: 'workflow.release-train',
        definition_version: 2,
        state: 'active',
        revision: 3,
        transitioned_at: 10,
      },
    });
    expect(ledger.get('workflow.release-train@2')?.disposition.state).toBe('active');
    expect(ledger.get('workflow.release-train@1')).toBeUndefined();
  });
});

function projection(overrides: Partial<WorkflowRunProjection> = {}): WorkflowRunProjection {
  const pinned = definition();
  return {
    run_id: 'run.release-train.1',
    definition: pinned,
    pinned_topology_digest: DIGEST_A,
    pinned_provider_registry_digest: DIGEST_A,
    status: 'running',
    sequence: 3,
    steps: {
      'fan-out': {
        status: 'succeeded',
        outputs: { finding: { output_name: 'finding', artifacts: [] } },
        placement_receipt: null,
        effect_receipt: {
          effect_digest: DIGEST_A,
          outcome: 'completed',
          output_set_digest: DIGEST_A,
          placement_digest: DIGEST_A,
          receipt_digest: DIGEST_A,
          run_id: 'run.release-train.1',
          step_id: 'fan-out',
        },
      },
      collect: { status: 'running', outputs: {}, placement_receipt: null, effect_receipt: null },
    },
    fan_out_plans: {},
    released_fan_out_attempts: [],
    settled_fan_out_attempts: [],
    history: [
      {
        run_id: 'run.release-train.1',
        sequence: 1,
        command_id: 'workflow-admit:1',
        input_digest: DIGEST_A,
        occurred_at: 1_000_000,
        event: {
          type: 'admitted',
          definition: pinned,
          pinned_topology_digest: DIGEST_A,
          pinned_provider_registry_digest: DIGEST_A,
          fan_out_plans: [],
        },
      },
      {
        run_id: 'run.release-train.1',
        sequence: 2,
        command_id: 'workflow-step:1',
        input_digest: DIGEST_A,
        occurred_at: 3_000_000,
        event: {
          type: 'step_started',
          step_id: 'fan-out',
          placement: {
            backend: 'codex_cli',
            configuration_digest: DIGEST_A,
            model: 'gpt-5',
            placement_digest: DIGEST_A,
            provider_registry_digest: DIGEST_A,
            route: { provider_id: 'provider.codex', route_id: 'route.default' },
            run_id: 'run.release-train.1',
            step_id: 'fan-out',
            topology_digest: DIGEST_A,
            worktree_placement: { kind: 'repository_local_root' },
          },
        },
      },
      {
        run_id: 'run.release-train.1',
        sequence: 3,
        command_id: 'workflow-step:2',
        input_digest: DIGEST_A,
        occurred_at: 8_500_000,
        event: {
          type: 'step_completed',
          step_id: 'fan-out',
          outputs: [],
          effect_receipt: {
            effect_digest: DIGEST_A,
            outcome: 'completed',
            output_set_digest: DIGEST_A,
            placement_digest: DIGEST_A,
            receipt_digest: DIGEST_A,
            run_id: 'run.release-train.1',
            step_id: 'fan-out',
          },
        },
      },
    ],
    ...overrides,
  };
}

describe('runTiming', () => {
  it('reads admission and the last event from the journal and refuses a duration for a live run', () => {
    const timing = runTiming(projection());
    expect(timing.admittedAt).toBe(1_000_000);
    expect(timing.lastEventAt).toBe(8_500_000);
    expect(timing.events).toBe(3);
    expect(timing.terminal).toBe(false);
    expect(timing.durationMicros).toBeNull();
    expect(timing.elapsedMicros).toBe(7_500_000);
  });

  it('gives a terminal run a duration and no elapsed figure', () => {
    const timing = runTiming(projection({ status: 'completed' }));
    expect(timing.durationMicros).toBe(7_500_000);
    expect(timing.elapsedMicros).toBeNull();
  });

  it('leaves every stamp null for a journal with no events', () => {
    const timing = runTiming(projection({ history: [] }));
    expect(timing).toMatchObject({
      admittedAt: null,
      firstEventAt: null,
      lastEventAt: null,
      events: 0,
      durationMicros: null,
      elapsedMicros: null,
    });
  });
});

describe('runStepSequence', () => {
  it('walks the pinned definition order and joins each step to its journal stamps', () => {
    const rows = runStepSequence(projection());
    expect(rows.map((row) => row.stepId)).toEqual(['fan-out', 'collect']);
    expect(rows[0]).toMatchObject({
      index: 1,
      status: 'succeeded',
      declared: true,
      operation: 'operation.work.start_attempt',
      startedAt: 3_000_000,
      settledAt: 8_500_000,
      durationMicros: 5_500_000,
      effect: 'completed',
      outputs: 1,
    });
    expect(rows[1]).toMatchObject({
      status: 'running',
      startedAt: null,
      durationMicros: null,
      effect: null,
      placement: null,
    });
  });

  it('marks a declared step the projection omits, and keeps an undeclared projection entry', () => {
    const rows = runStepSequence(
      projection({
        steps: {
          stray: { status: 'blocked', outputs: {}, placement_receipt: null, effect_receipt: null },
        },
      }),
    );
    expect(rows.map((row) => [row.stepId, row.status, row.declared])).toEqual([
      ['fan-out', 'absent', true],
      ['collect', 'absent', true],
      ['stray', 'blocked', false],
    ]);
  });

  it('counts succeeded steps from the projection', () => {
    expect(succeededStepCount(projection())).toBe(1);
  });
});

describe('formatDurationMicros', () => {
  it('prints hh:mm:ss and never lets a fast span read as no time', () => {
    expect(formatDurationMicros(null)).toBe('—');
    expect(formatDurationMicros(0)).toBe('00:00:00');
    expect(formatDurationMicros(400_000)).toBe('<00:00:01');
    expect(formatDurationMicros(336_000_000)).toBe('00:05:36');
    expect(formatDurationMicros(3_723_000_000)).toBe('01:02:03');
  });
});
