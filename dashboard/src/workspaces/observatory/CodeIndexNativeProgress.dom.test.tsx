import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CodeIndexFreshnessPayloadV1Schema } from '../../contracts/generated.ts';
import type {
  CodeIndexFreshnessPayloadV1,
  CodeIndexNativeGraphProgressV1,
  CodeIndexWorktreeFreshnessV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { IndexFreshness } from '../code/IndexFreshness.tsx';
import {
  CodeIndexPipeline,
  hasActiveCodeIndexBuild,
  hasLiveWorktreeProgress,
} from './CodeIndexPipeline.tsx';

type FreshnessResult = Extract<
  EnvelopeResult<CodeIndexFreshnessPayloadV1>,
  { outcome: 'envelope' }
>;

const GENERATION = 'generation.native-milestones';
const OBSERVED_AT = 1_753_003_600_000_000;
const clients: QueryClient[] = [];

function nativeProgress(
  overrides: Partial<CodeIndexNativeGraphProgressV1> = {},
): CodeIndexNativeGraphProgressV1 {
  return {
    generation_id: GENERATION,
    sealed_graph_digest: 'sha256:sealed-graph',
    daemon_incarnation: 2,
    producer_incarnation: 3,
    attempt_sequence: 4,
    progress_epoch: 9,
    phase: 'compact_segment_verified',
    encoded_entities: 90,
    total_entities: 90,
    encoded_relations: 40,
    total_relations: 40,
    committed_stage_pages: 0,
    committed_stage_mutations: 0,
    committed_stage_bytes: 0,
    committed_compact_segments: 1,
    committed_compact_bytes: 4096,
    last_progress_micros: OBSERVED_AT,
    active: false,
    ...overrides,
  };
}

function freshness(
  graph: CodeIndexNativeGraphProgressV1,
  overrides: Partial<CodeIndexWorktreeFreshnessV1> = {},
): FreshnessResult {
  const worktree: CodeIndexWorktreeFreshnessV1 = {
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
    native_graph_progress: graph,
    ...overrides,
  };
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
      domain_state: 'ready',
      legal_actions: [],
      // Exercise the generated wire decoder as well as the React consumer.
      payload: CodeIndexFreshnessPayloadV1Schema.parse({
        worktrees: [worktree],
        note: 'daemon scheduler state',
      }),
    },
  };
}

const VERIFIED_READING =
  'compact segment verified · 90 entities encoded · 40 relations encoded · 0 pages committed · 1 segments committed';

function renderPipeline(result: FreshnessResult) {
  return render(<CodeIndexPipeline result={result} pending={false} scopeKey="project" />);
}

afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('retained native graph milestones', () => {
  it('keeps verified work visible after the same-generation writer finishes without live polling', () => {
    const writing = freshness(nativeProgress({ active: true }));
    const { rerender } = renderPipeline(writing);
    expect(screen.getByText('Graph writer')).toBeTruthy();
    expect(hasActiveCodeIndexBuild(writing)).toBe(true);

    const finished = freshness(nativeProgress());
    const original = structuredClone(finished);
    rerender(<CodeIndexPipeline result={finished} pending={false} scopeKey="project" />);
    expect(screen.getByText('Graph milestones')).toBeTruthy();
    expect(screen.getByText(VERIFIED_READING)).toBeTruthy();
    expect(screen.queryByText('Graph writer')).toBeNull();
    expect(screen.getByText('no active code-index build')).toBeTruthy();
    expect(hasLiveWorktreeProgress(finished.envelope.payload.worktrees[0]!)).toBe(false);
    expect(hasActiveCodeIndexBuild(finished)).toBe(false);
    expect(finished).toEqual(original);
  });

  it('retains actual stage pages after an interrupted writer without claiming a verified segment', () => {
    const result = freshness(nativeProgress({
      phase: 'stage_page_commit',
      encoded_entities: 0,
      encoded_relations: 0,
      committed_stage_pages: 2,
      committed_stage_mutations: 70,
      committed_stage_bytes: 4096,
      committed_compact_segments: 0,
      committed_compact_bytes: 0,
    }), {
      code_graph_serving: { state: 'refused', reason: 'resident memory' },
    });
    renderPipeline(result);
    expect(screen.getByText('Graph milestones')).toBeTruthy();
    expect(screen.getByText(
      'stage page commit · 0 entities encoded · 0 relations encoded · 2 pages committed · 0 segments committed',
    )).toBeTruthy();
    expect(screen.getByText('refused · resident memory')).toBeTruthy();
    expect(hasActiveCodeIndexBuild(result)).toBe(false);
  });

  it.each(['generation.successor', null])(
    'removes retained milestones when the current generation becomes %s',
    (latestGeneration) => {
      const { rerender } = renderPipeline(freshness(nativeProgress()));
      expect(screen.getByText(VERIFIED_READING)).toBeTruthy();
      rerender(<CodeIndexPipeline
        result={freshness(nativeProgress(), { latest_generation_id: latestGeneration })}
        pending={false}
        scopeKey="project"
      />);
      expect(screen.queryByText('Graph milestones')).toBeNull();
      expect(screen.queryByText(VERIFIED_READING)).toBeNull();
    },
  );

  it.each(['compact_encoding', 'compact_segment_verified'] as const)(
    'does not turn an inactive %s label or encoded rows into committed work',
    (phase) => {
      const result = freshness(nativeProgress({
        phase,
        committed_compact_segments: 0,
        committed_compact_bytes: 0,
      }));
      renderPipeline(result);
      expect(screen.queryByText('Graph milestones')).toBeNull();
      expect(screen.queryByText(/entities encoded/)).toBeNull();
      expect(hasActiveCodeIndexBuild(result)).toBe(false);
    },
  );

  it('keeps an active successor visible while the previous generation is still current', () => {
    const result = freshness(nativeProgress({
      generation_id: 'generation.successor',
      active: true,
      phase: 'compact_encoding',
      committed_compact_segments: 0,
      committed_compact_bytes: 0,
    }));
    renderPipeline(result);
    expect(screen.getByText('Graph writer')).toBeTruthy();
    expect(screen.queryByText('Graph milestones')).toBeNull();
    expect(screen.getByText(
      'compact encoding · 90 entities encoded · 40 relations encoded · 0 pages committed · 0 segments committed',
    )).toBeTruthy();
    expect(hasActiveCodeIndexBuild(result)).toBe(true);
  });

  it('renders the same retained counts through the HTTP freshness panel on its idle cadence', async () => {
    vi.useFakeTimers();
    const result = freshness(nativeProgress());
    const fetch = vi.fn(async () => new Response(JSON.stringify(result.envelope), { status: 200 }));
    vi.stubGlobal('fetch', fetch);
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, gcTime: Infinity } },
    });
    clients.push(client);
    render(<QueryClientProvider client={client}><IndexFreshness /></QueryClientProvider>);
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(screen.getByText('Graph milestones')).toBeTruthy();
    expect(screen.getByText(VERIFIED_READING)).toBeTruthy();
    expect(fetch).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1_001); });
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(screen.getByText(VERIFIED_READING)).toBeTruthy();
    await act(async () => { await vi.advanceTimersByTimeAsync(28_999); });
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(screen.getByText(VERIFIED_READING)).toBeTruthy();
  });
});
