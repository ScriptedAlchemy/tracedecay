/**
 * What the board is allowed to say about a task.
 *
 * `WorkProjection` carries no status field, so every stage here has to be a
 * reading of a field the contract really has. The test that matters most is the
 * last one: a stage must never be inferred from the absence of information.
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import type { WorkProjection } from '../../contracts/index.ts';

import {
  availableCommands,
  coverageReading,
  stageState,
  workStage,
  type WorkStage,
} from './workModel.ts';
import { WITHHELD_ATTEMPT_OPERATION_KEYS } from './WorkPage.tsx';

/** `crates/tracedecay-api/src/work.rs`, the descriptor both this dashboard and
 * the daemon's router are derived from. */
const WORK_DESCRIPTOR = fileURLToPath(
  new URL('../../../../crates/tracedecay-api/src/work.rs', import.meta.url),
);

/**
 * The attempt operation ids, read out of the canonical Rust descriptor.
 *
 * The dashboard has no generated inventory of these: the operation catalog is
 * not in the contract bundle, so nothing in `src/contracts/generated.ts` names
 * them. Parsing the descriptor is what is available that cannot silently
 * agree with a stale copy — a hand list in this file would drift in exactly
 * the same direction as the hand list it is checking, which is how the page
 * came to name eight of nine.
 */
function canonicalAttemptOperationKeys(): string[] {
  const source = readFileSync(WORK_DESCRIPTOR, 'utf8');
  const block = /pub const ATTEMPT: \[Self; (\d+)\] = \[([\s\S]*?)\];/.exec(source);
  if (!block) throw new Error(`no ATTEMPT constant found in ${WORK_DESCRIPTOR}`);
  const variants = [...block[2]!.matchAll(/Self::(\w+)/g)].map((match) => match[1]!);
  // The declared arity and the variants actually listed must agree, or this
  // helper would quietly under-report the very drift it exists to catch.
  expect(variants).toHaveLength(Number(block[1]));
  return variants.map((variant) =>
    variant.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase(),
  );
}

function projection(overrides: Partial<WorkProjection> = {}): WorkProjection {
  return {
    accepted_proposal: null,
    authority: {
      actor_id: 'actor',
      policy_digest: 'digest',
      project_id: 'project',
      repository_id: 'repository',
      worktree_id: 'worktree',
    },
    dependencies: [],
    execution_admitted: false,
    history_len: 1,
    runtime_evidence: [],
    task_accepted: false,
    task_id: 'task-1',
    title: 'A task',
    version: 1,
    ...overrides,
  };
}

const evidence = (terminal: boolean) => ({
  evidence_digest: 'digest',
  run_id: 'run-1',
  terminal,
});

describe('the stage a projection reads as', () => {
  it('reads each gate the contract records', () => {
    expect(workStage(projection())).toBe('proposal_open');
    expect(workStage(projection({ accepted_proposal: 'proposal-1' }))).toBe('proposal_accepted');
    expect(workStage(projection({ accepted_proposal: 'p', task_accepted: true }))).toBe(
      'task_accepted',
    );
    expect(
      workStage(projection({ accepted_proposal: 'p', task_accepted: true, execution_admitted: true })),
    ).toBe('execution_admitted');
  });

  /** Terminal evidence outranks admission: a finished task is finished even
   * though `execution_admitted` is still true underneath it. */
  it('reports terminal evidence as the furthest gate', () => {
    expect(
      workStage(
        projection({
          accepted_proposal: 'p',
          task_accepted: true,
          execution_admitted: true,
          runtime_evidence: [evidence(false), evidence(true)],
        }),
      ),
    ).toBe('evidence_terminal');
  });

  /** Non-terminal evidence is work in flight, not a result. */
  it('does not treat non-terminal evidence as a result', () => {
    expect(
      workStage(
        projection({ execution_admitted: true, task_accepted: true, runtime_evidence: [evidence(false)] }),
      ),
    ).toBe('execution_admitted');
  });

  it('calls only a terminal task ready, and never calls a task complete-with-nothing', () => {
    const states = (['proposal_open', 'proposal_accepted', 'task_accepted', 'execution_admitted'] as const).map(
      stageState,
    );
    expect(new Set(states)).toEqual(new Set(['partial']));
    expect(stageState('evidence_terminal')).toBe('ready');
    for (const stage of [
      'proposal_open',
      'proposal_accepted',
      'task_accepted',
      'execution_admitted',
      'evidence_terminal',
    ] satisfies WorkStage[]) {
      expect(stageState(stage)).not.toBe('complete_zero_findings');
    }
  });
});

describe('which commands a task may be offered', () => {
  it('offers proposal review and acceptance only while the proposal is open', () => {
    expect(availableCommands(projection())).toEqual(
      expect.arrayContaining(['review_proposal', 'accept_proposal']),
    );
    expect(availableCommands(projection({ accepted_proposal: 'p' }))).not.toEqual(
      expect.arrayContaining(['accept_proposal']),
    );
  });

  it('offers admission only after the task is accepted, and once', () => {
    expect(availableCommands(projection({ accepted_proposal: 'p' }))).not.toContain(
      'admit_execution',
    );
    expect(availableCommands(projection({ accepted_proposal: 'p', task_accepted: true }))).toContain(
      'admit_execution',
    );
    expect(
      availableCommands(
        projection({ accepted_proposal: 'p', task_accepted: true, execution_admitted: true }),
      ),
    ).not.toContain('admit_execution');
  });

  it('offers evidence attachment only once execution is admitted', () => {
    expect(availableCommands(projection())).not.toContain('attach_runtime_evidence');
    expect(availableCommands(projection({ execution_admitted: true }))).toContain(
      'attach_runtime_evidence',
    );
  });

  /**
   * No control may be drawn for a runtime-attempt operation. The daemon
   * asserts it does not expose them (`src/dashboard/work_api.rs`), so a
   * control for one could only ever fail.
   *
   * The candidate set is the canonical one read out of the descriptor rather
   * than a list written here. The list this replaced held six names, two of
   * which — `terminalize` and a `heartbeat` that is not an operation at all —
   * meant it could not have caught four of the nine.
   */
  it('offers no attempt operation, at any stage', () => {
    const attempts = canonicalAttemptOperationKeys().map((key) =>
      key.slice('attempt_'.length),
    );
    expect(attempts).toHaveLength(9);
    for (const candidate of [
      projection(),
      projection({ accepted_proposal: 'p' }),
      projection({ accepted_proposal: 'p', task_accepted: true }),
      projection({ accepted_proposal: 'p', task_accepted: true, execution_admitted: true }),
    ]) {
      for (const command of availableCommands(candidate)) {
        expect(attempts, `${command} is an attempt operation`).not.toContain(command);
      }
    }
  });
});

/**
 * The page prints this set and counts it in the same sentence, so a short list
 * is a false statement twice over: it names fewer withheld operations than
 * exist, and prints a total to match.
 */
describe('the withheld runtime-attempt inventory the page prints', () => {
  it('is the canonical set, in the descriptor’s order', () => {
    expect([...WITHHELD_ATTEMPT_OPERATION_KEYS]).toEqual(canonicalAttemptOperationKeys());
  });

  it('includes the operation that ends an attempt', () => {
    // Named on its own because this is the one that was missing, and an
    // order-and-contents comparison would not say so when it fails.
    expect(WITHHELD_ATTEMPT_OPERATION_KEYS).toContain('attempt_finish');
  });
});

describe('how much of the board is being shown', () => {
  it('separates an empty complete board from a withheld one', () => {
    expect(coverageReading({ state: 'complete', returned: 0, total: 0 })).toMatchObject({
      state: 'complete_zero_findings',
    });
    expect(coverageReading({ state: 'complete', returned: 4, total: 4 })).toMatchObject({
      state: 'ready',
    });
  });

  /** A capped or partial page is the daemon saying there is more. Reporting it
   * as ready would present a fraction of the board as the whole of it. */
  it('never rounds a capped or partial page up to a complete board', () => {
    const capped = coverageReading({
      state: 'capped',
      cap: 10,
      returned: 10,
      total: 97,
      cursor: { generation_id: 'g', token: 't' },
      range: { start_exclusive: 0, end_inclusive: 10 },
    });
    expect(capped.state).toBe('partial');
    expect(capped.detail).toContain('97');

    expect(
      coverageReading({
        state: 'partial',
        returned: 3,
        total: 9,
        cursor: { generation_id: 'g', token: 't' },
        range: { start_exclusive: 0, end_inclusive: 3 },
      }).state,
    ).toBe('partial');
  });
});
