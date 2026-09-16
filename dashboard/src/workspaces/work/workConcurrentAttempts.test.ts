import { describe, expect, it } from 'vitest';
import { FeedbackProximityEncounterV1Schema } from '../../contracts/index.ts';
import { joinConcurrentEncounter } from './workConcurrentAttempts.ts';

const scope = {
  project_id: 'project.concurrent',
  repository_id: 'repository.concurrent',
  worktree_id: 'worktree.concurrent',
  branch_ref: 'refs/heads/concurrent',
  head_commit_id: 'commit.concurrent',
};

function participant(provider: string, session: string, agent: string) {
  return {
    access: 'write',
    activity: { start: 10, end: 20 },
    address: {
      scope,
      file: 'file.concurrent',
      span: { start_byte: 4, end_byte: 12 },
      symbol: 'symbol.concurrent',
    },
    agent_id: agent,
    branch_ref: 'refs/heads/concurrent',
    head_revision: `commit.${session}`,
    source: { provider, session_id: session, source_key: null },
    worktree_id: 'worktree.concurrent',
    worktree_root: `/tmp/${session}`,
  };
}

const encounter = FeedbackProximityEncounterV1Schema.parse({
  coverage: 'complete',
  encounter_id: 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
  expires_at: 30,
  interval: { start: 10, end: 20 },
  observed_at: 20,
  participants: [
    participant('codex', 'session.left', 'agent.left'),
    participant('cursor', 'session.right', 'agent.right'),
  ],
  relation: { relation_kind: 'overlapping_edit', warning_class: 'same_file' },
  scope,
});

describe('concurrent attempt joins', () => {
  it('stays partial when exact provider and session joins are missing', () => {
    const joined = joinConcurrentEncounter(encounter, new Map(), new Map());

    expect(joined).toEqual({ row: null, missingJoins: 2 });
  });

  it('does not substitute planned attempt identity for observed execution spans', () => {
    const identities = new Map([
      [
        JSON.stringify(['codex', 'session.left']),
        { task_id: 'task', run_id: 'run', attempt_id: 'left' },
      ],
      [
        JSON.stringify(['cursor', 'session.right']),
        { task_id: 'task', run_id: 'run', attempt_id: 'right' },
      ],
    ]);

    const joined = joinConcurrentEncounter(encounter, identities, new Map());

    expect(joined).toEqual({ row: null, missingJoins: 2 });
  });
});
