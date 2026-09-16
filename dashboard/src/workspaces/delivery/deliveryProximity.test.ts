import { describe, expect, it } from 'vitest';
import type {
  DeliveryAttentionItemV1,
  DeliveryInboxPullRequestV1,
  DeliveryInboxV1,
} from '../../contracts/generated.ts';
import type {
  FeedbackProximityEncounterV1,
  FeedbackProximityReadResultV1,
} from '../../contracts/index.ts';
import { applyProximityAttention } from './deliveryProximity.ts';

const MATCHED_HEAD = 'a'.repeat(40);
const UNMATCHED_HEAD = 'b'.repeat(40);

function attentionItem(source: DeliveryAttentionItemV1['source']): DeliveryAttentionItemV1 {
  return {
    id: `project.alpha:42:${source}`,
    project_id: 'project.alpha',
    pull_request_id: '42',
    source,
    state: 'unavailable',
    evidence: [],
    coverage: 'unsupported',
    observed_at_micros: null,
  };
}

function pullRequest(indexedHeadCommitId: string): DeliveryInboxPullRequestV1 {
  return {
    id: 'project.alpha:github:42',
    project_id: 'project.alpha',
    repository_id: 'repository.alpha',
    worktree_id: 'worktree.alpha',
    branch_ref: 'refs/heads/feature/delivery',
    indexed_head_commit_id: indexedHeadCommitId,
    indexed_generation: 'generation.alpha.1',
    state: 'current',
    pull_request: {
      id: 'github:42',
      label: 'Pull request #42',
      provider: 'github',
      pull_request_id: '42',
      identity: null,
      operations: [],
    },
    attention: [
      attentionItem('ci_failure'),
      attentionItem('overlapping_edit'),
      attentionItem('confirmed_conflict'),
      attentionItem('divergent_shared_implementation'),
    ],
    shared_code: [],
  };
}

function inbox(pullRequests: DeliveryInboxPullRequestV1[]): DeliveryInboxV1 {
  return {
    registry_state: 'ready',
    projects: [],
    pull_requests: pullRequests,
    membership_edges: [],
    omitted_projects: 0,
    excluded_pull_requests: 0,
  };
}

type ProximityRelationKind = 'overlapping_edit' | 'confirmed_conflict' | 'shared_code_candidate';

function relationFor(
  relationKind: ProximityRelationKind,
  headRevision: string,
): FeedbackProximityEncounterV1['relation'] {
  switch (relationKind) {
    case 'overlapping_edit':
      return { relation_kind: 'overlapping_edit', warning_class: 'same_file' };
    case 'confirmed_conflict':
      return {
        relation_kind: 'confirmed_conflict',
        conflict_handle: {
          common_base_revision: 'commit.base',
          differences: [],
          evidence_digest: 'sha256:evidence',
          left_head_revision: headRevision,
          right_head_revision: UNMATCHED_HEAD,
        },
      };
    case 'shared_code_candidate':
      return {
        relation_kind: 'shared_code_candidate',
        warning_class: 'same_symbol',
        clone_handle: {
          retrieval_anchor_ids: [],
          source_generation: 'generation.proximity.1',
          source_symbol: 'symbol.shared',
        },
      };
    default: {
      const unhandled: never = relationKind;
      return unhandled;
    }
  }
}

function encounter(
  relationKind: ProximityRelationKind,
  headRevision: string,
  overrides: Partial<FeedbackProximityEncounterV1> = {},
): FeedbackProximityEncounterV1 {
  const scope = {
    project_id: 'project.alpha',
    repository_id: 'repository.alpha',
    worktree_id: 'worktree.alpha',
    branch_ref: 'refs/heads/feature/delivery',
    head_commit_id: headRevision,
  };
  return {
    encounter_id: `sha256:${relationKind}`,
    scope,
    interval: { start: 1_700_000_000_000_000, end: 1_700_000_100_000_000 },
    participants: [
      {
        source: { provider: 'cursor', session_id: 'sess-a', source_key: null },
        agent_id: 'agent-a',
        worktree_id: 'worktree.alpha',
        worktree_root: '/tmp/alpha',
        branch_ref: 'refs/heads/feature/delivery',
        head_revision: headRevision,
        access: 'write',
        activity: { start: 1_700_000_000_000_000, end: 1_700_000_050_000_000 },
        address: {
          scope,
          file: 'src/lib.rs',
          span: { start_byte: 0, end_byte: 10 },
          symbol: 'symbol',
        },
      },
    ],
    relation: relationFor(relationKind, headRevision),
    observed_at: 1_700_000_100_000_000,
    expires_at: 1_700_000_400_000_000,
    coverage: 'complete',
    ...overrides,
  };
}

function readyResult(encounters: FeedbackProximityEncounterV1[]): FeedbackProximityReadResultV1 {
  return {
    state: 'complete',
    page: {
      scope: {
        project_id: 'project.alpha',
        repository_id: 'repository.alpha',
        worktree_id: 'worktree.alpha',
        branch_ref: 'refs/heads/feature/delivery',
        head_commit_id: MATCHED_HEAD,
      },
      source_generation: 'generation.proximity.1',
      observed_at: 1_700_000_200_000_000,
      expires_at: 1_700_000_500_000_000,
      encounters,
    },
  };
}

describe('applyProximityAttention', () => {
  it('leaves server-served attention untouched when proximity has not resolved', () => {
    const base = inbox([pullRequest(MATCHED_HEAD)]);
    expect(applyProximityAttention(base, undefined)).toBe(base);
  });

  it('marks overlapping_edit active with proximity evidence for a head-matched encounter', () => {
    const result = readyResult([encounter('overlapping_edit', MATCHED_HEAD)]);
    const [pr] = applyProximityAttention(inbox([pullRequest(MATCHED_HEAD)]), result).pull_requests;
    const overlap = pr!.attention.find((item) => item.source === 'overlapping_edit')!;
    expect(overlap.state).toBe('active');
    expect(overlap.coverage).toBe('complete');
    expect(overlap.evidence).toEqual([
      { kind: 'proximity_encounter', encounter_id: 'sha256:overlapping_edit', relation_kind: 'overlapping_edit' },
    ]);
  });

  it('marks confirmed_conflict active with proximity evidence for a head-matched encounter', () => {
    const result = readyResult([encounter('confirmed_conflict', MATCHED_HEAD)]);
    const [pr] = applyProximityAttention(inbox([pullRequest(MATCHED_HEAD)]), result).pull_requests;
    const conflict = pr!.attention.find((item) => item.source === 'confirmed_conflict')!;
    expect(conflict.state).toBe('active');
    expect(conflict.evidence).toEqual([
      { kind: 'proximity_encounter', encounter_id: 'sha256:confirmed_conflict', relation_kind: 'confirmed_conflict' },
    ]);
  });

  it('maps shared_code_candidate onto divergent_shared_implementation', () => {
    const result = readyResult([encounter('shared_code_candidate', MATCHED_HEAD)]);
    const [pr] = applyProximityAttention(inbox([pullRequest(MATCHED_HEAD)]), result).pull_requests;
    const divergent = pr!.attention.find((item) => item.source === 'divergent_shared_implementation')!;
    expect(divergent.state).toBe('active');
    expect(divergent.evidence).toEqual([
      {
        kind: 'proximity_encounter',
        encounter_id: 'sha256:shared_code_candidate',
        relation_kind: 'shared_code_candidate',
      },
    ]);
  });

  it('does not attach an encounter whose participant head does not match the indexed head', () => {
    const result = readyResult([encounter('overlapping_edit', UNMATCHED_HEAD)]);
    const [pr] = applyProximityAttention(inbox([pullRequest(MATCHED_HEAD)]), result).pull_requests;
    const overlap = pr!.attention.find((item) => item.source === 'overlapping_edit')!;
    expect(overlap.state).toBe('clear');
    expect(overlap.coverage).toBe('complete');
    expect(overlap.evidence).toEqual([]);
  });

  it('clears a proximity source to complete zero when the page carries no matching encounter', () => {
    const result = readyResult([]);
    const [pr] = applyProximityAttention(inbox([pullRequest(MATCHED_HEAD)]), result).pull_requests;
    for (const source of ['overlapping_edit', 'confirmed_conflict', 'divergent_shared_implementation']) {
      const item = pr!.attention.find((entry) => entry.source === source)!;
      expect(item.state).toBe('clear');
      expect(item.coverage).toBe('complete');
    }
    // Server-served sources outside the proximity join are left alone.
    expect(pr!.attention.find((entry) => entry.source === 'ci_failure')!.state).toBe('unavailable');
  });

  it('marks the proximity sources unavailable when the read is unavailable', () => {
    const result: FeedbackProximityReadResultV1 = {
      state: 'unavailable',
      observed_at: 1_700_000_200_000_000,
    };
    const [pr] = applyProximityAttention(inbox([pullRequest(MATCHED_HEAD)]), result).pull_requests;
    for (const source of ['overlapping_edit', 'confirmed_conflict', 'divergent_shared_implementation']) {
      const item = pr!.attention.find((entry) => entry.source === source)!;
      expect(item.state).toBe('unavailable');
      expect(item.coverage).toBe('unavailable');
    }
  });

  it('marks the proximity sources unavailable when the read is denied', () => {
    const result: FeedbackProximityReadResultV1 = {
      state: 'denied',
      observed_at: 1_700_000_200_000_000,
    };
    const [pr] = applyProximityAttention(inbox([pullRequest(MATCHED_HEAD)]), result).pull_requests;
    for (const source of ['overlapping_edit', 'confirmed_conflict', 'divergent_shared_implementation']) {
      expect(pr!.attention.find((entry) => entry.source === source)!.state).toBe('unavailable');
    }
  });

  it('preserves partial coverage when the proximity read is partial', () => {
    const complete = readyResult([encounter('overlapping_edit', MATCHED_HEAD)]);
    const result: FeedbackProximityReadResultV1 = {
      state: 'partial',
      page: complete.page,
      omissions: ['encounter_limit'],
    };
    const [pr] = applyProximityAttention(inbox([pullRequest(MATCHED_HEAD)]), result).pull_requests;
    const overlap = pr!.attention.find((item) => item.source === 'overlapping_edit')!;
    expect(overlap.state).toBe('active');
    expect(overlap.coverage).toBe('partial');
    const clear = pr!.attention.find((item) => item.source === 'confirmed_conflict')!;
    expect(clear.state).toBe('clear');
    expect(clear.coverage).toBe('partial');
  });

  it('preserves stale coverage when the proximity read is stale', () => {
    const complete = readyResult([]);
    const result: FeedbackProximityReadResultV1 = {
      state: 'stale',
      page: complete.page,
      omissions: ['code_index_revision_mismatch'],
    };
    const [pr] = applyProximityAttention(inbox([pullRequest(MATCHED_HEAD)]), result).pull_requests;
    for (const source of ['overlapping_edit', 'confirmed_conflict', 'divergent_shared_implementation']) {
      const item = pr!.attention.find((entry) => entry.source === source)!;
      expect(item.state).toBe('clear');
      expect(item.coverage).toBe('stale');
    }
  });
});
