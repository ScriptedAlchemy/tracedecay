import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { CodeIndexFreshnessPayloadV1Schema } from '../../contracts/generated.ts';
import type {
  CodeIndexBuildProgressV1,
  CodeIndexFreshnessPayloadV1,
  CodeIndexWorktreeFreshnessV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { CodeIndexPipeline, hasActiveCodeIndexBuild } from './CodeIndexPipeline.tsx';

type FreshnessResult = Extract<
  EnvelopeResult<CodeIndexFreshnessPayloadV1>,
  { outcome: 'envelope' }
>;

const GENERATION = 'generation.progress';
const OBSERVED_AT = 1_753_003_600_000_000;

function progress(overrides: Partial<CodeIndexBuildProgressV1> = {}): CodeIndexBuildProgressV1 {
  return {
    generation_id: GENERATION,
    daemon_incarnation: 1,
    producer_incarnation: 1,
    progress_epoch: 1,
    sealed_source_digest: 'sha256:sealed-source',
    phase: 'bulk_commit',
    committed_pages: 2,
    committed_chunks: 2,
    committed_imports: 0,
    committed_payload_bytes: 128,
    completed_files: 2,
    total_files: 10,
    completed_lexical_units: 2,
    total_lexical_units: 10,
    current_batch_pages: 0,
    current_batch_payload_bytes: 0,
    elapsed_micros: 1_000_000,
    last_commit_latency_micros: 100,
    files_per_second: null,
    lexical_units_per_second: null,
    estimated_remaining_seconds: null,
    last_progress_micros: OBSERVED_AT,
    blocked_reason: null,
    ...overrides,
  };
}

function worktree(
  overrides: Partial<CodeIndexWorktreeFreshnessV1> = {},
): CodeIndexWorktreeFreshnessV1 {
  return {
    worktree_root: '/project',
    repository_id: 'repository.project',
    worktree_id: 'worktree.primary',
    source_reference: 'refs/heads/master',
    source_revision: null,
    latest_generation_id: GENERATION,
    code_graph_serving: { state: 'ready' },
    clone_index: null,
    snapshot_content_identity: 'sha256:source',
    sealed_at_micros: OBSERVED_AT,
    last_reconcile_micros: OBSERVED_AT,
    staleness_state: 'fresh',
    rebuild_in_flight: false,
    hook_hint_count: 0,
    coverage: 'complete',
    progress: null,
    restore_progress: null,
    parked: null,
    ...overrides,
  };
}

function freshness(
  overrides: Partial<CodeIndexWorktreeFreshnessV1> = {},
  domainState: 'ready' | 'loading' = 'ready',
): FreshnessResult {
  return {
    outcome: 'envelope',
    envelope: {
      schema_revision: 1,
      scope: { project_id: 'project', storage_mode: 'project', store_root: '/store' },
      version: { entity_version: null, graph_version: null },
      time: { valid_time_micros: null, observation_time_micros: OBSERVED_AT },
      source_watermark: null,
      authorization: { outcome: 'authorized' },
      coverage: {
        completeness: 'complete',
        eligible: 1,
        examined: 1,
        matched: 1,
        excluded: 0,
        omitted: 0,
        unknown: 0,
        denominator: 1,
        unit: 'mounted_worktree',
        omission_reasons: [],
      },
      freshness: { state: 'fresh', observed_at_micros: OBSERVED_AT, watermark: null },
      domain_state: domainState,
      legal_actions: [],
      payload: {
        worktrees: [worktree(overrides)],
        note: 'daemon scheduler state',
      },
    },
  };
}

const restoreProgress = {
  generation_id: GENERATION,
  artifact_digest: 'sha256:artifact',
  authenticated_completed: 2,
  authenticated_total: 5,
  authenticated_remaining: 3,
};

afterEach(cleanup);

describe('code-index activity polling', () => {
  it('shows pre-seal work before the first text page commits', () => {
    const result = freshness({ preseal_attempt: {
      daemon_incarnation: 1, producer_incarnation: 2, attempt_sequence: 3,
      progress_epoch: 4, phase: 'resolution', completed_items: 7,
      total_items: null, last_progress_micros: OBSERVED_AT, active: true,
    } });
    result.envelope.payload = CodeIndexFreshnessPayloadV1Schema.parse(result.envelope.payload);
    expect(hasActiveCodeIndexBuild(result)).toBe(true);
    render(<CodeIndexPipeline result={result} pending={false} scopeKey="project" />);
    expect(screen.getByText('resolution · 7 items')).toBeTruthy();
    expect(screen.queryByText('no active code-index build')).toBeNull();
  });

  it('keeps polling an in-flight rebuild before its first committed report', () => {
    expect(hasActiveCodeIndexBuild(freshness({ rebuild_in_flight: true }))).toBe(true);
  });

  it('does not let the previous ready report hide an in-flight successor', () => {
    expect(hasActiveCodeIndexBuild(freshness({
      rebuild_in_flight: true,
      progress: progress({ phase: 'ready' }),
    }))).toBe(true);
  });

  it('keeps polling a restore even when serving readiness is ready', () => {
    expect(hasActiveCodeIndexBuild(freshness({ restore_progress: restoreProgress }))).toBe(true);
  });

  it('observes active work after an idle worktree in an aggregate read', () => {
    const result = freshness();
    result.envelope.payload.worktrees.push(worktree({
      worktree_root: '/project/linked',
      rebuild_in_flight: true,
    }));
    expect(hasActiveCodeIndexBuild(result)).toBe(true);
  });

  it('keeps polling measured graph publication', () => {
    expect(hasActiveCodeIndexBuild(freshness({
      progress: progress({ phase: 'graph_publication' }),
    }))).toBe(true);
  });

  it('keeps polling a loading envelope before a worktree mounts', () => {
    const result = freshness({}, 'loading');
    result.envelope.payload.worktrees = [];
    expect(hasActiveCodeIndexBuild(result)).toBe(true);
  });

  it('stops active polling after the generation settles', () => {
    expect(hasActiveCodeIndexBuild(freshness({
      progress: progress({ phase: 'ready' }),
    }))).toBe(false);
  });

  it('stops active polling after the final report retires', () => {
    expect(hasActiveCodeIndexBuild(freshness())).toBe(false);
  });

  it('does not invent activity for a ready empty scope', () => {
    const result = freshness();
    result.envelope.payload.worktrees = [];
    expect(hasActiveCodeIndexBuild(result)).toBe(false);
  });

  it('does not invent activity before a response arrives', () => {
    expect(hasActiveCodeIndexBuild(undefined)).toBe(false);
  });

  it('does not turn a transport failure into claimed build activity', () => {
    expect(hasActiveCodeIndexBuild({ outcome: 'transport', state: 'error' })).toBe(false);
  });
});

describe('code-index progress transitions', () => {
  it('renders unreported capture, committed progress, and completion without inventing counters', async () => {
    const cold = freshness({
      latest_generation_id: null,
      snapshot_content_identity: null,
      sealed_at_micros: null,
      rebuild_in_flight: true,
      staleness_state: 'indexing',
      code_graph_serving: { state: 'pending' },
    }, 'loading');
    const view = render(<CodeIndexPipeline result={cold} pending={false} scopeKey="project" />);
    expect(screen.getByText('code-index build in progress; progress not yet reported')).toBeTruthy();
    expect(screen.queryByText('no active code-index build')).toBeNull();
    expect(screen.queryByRole('progressbar')).toBeNull();

    const building = freshness({ rebuild_in_flight: true, progress: progress() }, 'loading');
    view.rerender(<CodeIndexPipeline result={building} pending={false} scopeKey="project" />);
    expect(await screen.findByText('bulk commit · 20.0%')).toBeTruthy();
    expect(screen.getByRole('progressbar').getAttribute('value')).toBe('20');
    expect(screen.queryByText('code-index build in progress; progress not yet reported')).toBeNull();

    const completed = freshness({
      progress: progress({
        phase: 'ready',
        progress_epoch: 2,
        committed_pages: 10,
        committed_chunks: 10,
        completed_files: 10,
        completed_lexical_units: 10,
      }),
    });
    view.rerender(<CodeIndexPipeline result={completed} pending={false} scopeKey="project" />);
    expect(await screen.findByText('ready · 100.0%')).toBeTruthy();
    expect(hasActiveCodeIndexBuild(completed)).toBe(false);

    view.rerender(<CodeIndexPipeline result={freshness()} pending={false} scopeKey="project" />);
    expect(await screen.findByText('no active code-index build')).toBeTruthy();
    expect(screen.queryByRole('progressbar')).toBeNull();
  });

  it('keeps restore checks separate from build counters and stops polling after restoration', async () => {
    const restoring = freshness({ restore_progress: restoreProgress });
    const view = render(<CodeIndexPipeline result={restoring} pending={false} scopeKey="project" />);
    expect(screen.getByText('restoring lexical artifact')).toBeTruthy();
    expect(screen.getByText('3 authentication checks remaining')).toBeTruthy();
    expect(screen.getByRole('progressbar').getAttribute('value')).toBe('2');
    expect(screen.getByRole('progressbar').getAttribute('max')).toBe('5');
    expect(screen.queryByText('no active code-index build')).toBeNull();
    expect(hasActiveCodeIndexBuild(restoring)).toBe(true);

    const restored = freshness();
    view.rerender(<CodeIndexPipeline result={restored} pending={false} scopeKey="project" />);
    expect(await screen.findByText('no active code-index build')).toBeTruthy();
    expect(screen.queryByRole('progressbar')).toBeNull();
    expect(hasActiveCodeIndexBuild(restored)).toBe(false);
  });
});
