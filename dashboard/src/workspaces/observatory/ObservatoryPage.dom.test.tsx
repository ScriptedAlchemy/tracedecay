import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useScope } from '../../data/scope/store.ts';
import { ObservatoryPage } from './ObservatoryPage.tsx';

/**
 * Store-telemetry rendering against the current `/api/storage/telemetry`
 * contract (src/dashboard/storage_telemetry_api.rs).
 *
 * The endpoint's honesty rules are the assertions here: an unconfigured budget
 * is a missing owner *setting* and never reads as unsupported or as a pass;
 * size is live but growth stays explicitly unknown until an execution-owned
 * sampler exists; and roles that share one database appear once, naming every
 * role.
 */

const SETTING_KEY = 'sync.retention.v1 store_soft_budgets_bytes';
/** Two watermarks an hour apart, so the rendered pair is a real UTC instant
 * rather than the epoch and the two ends are distinguishable. */
const SAMPLE_PREVIOUS_MICROS = 1_753_000_000_000_000;
const SAMPLE_CURRENT_MICROS = 1_753_003_600_000_000;
const SAMPLE_PREVIOUS_ISO = '2025-07-20T08:26:40.000Z';
const SAMPLE_CURRENT_ISO = '2025-07-20T09:26:40.000Z';

describe('ObservatoryPage store telemetry', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  afterEach(() => {
    useScope.getState().selectAllProjects();
    vi.useRealTimers();
  });

  it('renders every budget and growth state honestly, and merges shared-file roles', async () => {
    stubTelemetry(telemetryPayload());
    renderObservatory();

    // Shared store file: one card, both roles named.
    const shared = await screen.findByText('graph · memory (shared store file)');
    expect(shared).toBeTruthy();

    // Evaluated · within budget shows the real observed size and soft limit.
    expect(screen.getByText(/within budget · 204\.7 MiB of 512\.0 MiB soft limit/)).toBeTruthy();
    // Evaluated · over budget shows the real overage.
    expect(
      screen.getByText(/over budget · 704\.0 MiB of 512\.0 MiB soft limit · over by 192\.0 MiB/),
    ).toBeTruthy();

    // Unset: a missing setting, named exactly, and never "unsupported".
    const unsetRow = document.querySelector('[data-dimension-state="unset"]');
    expect(unsetRow?.textContent).toContain(`no budget configured · set ${SETTING_KEY}`);
    // The setting is a mono token, so a missing setting is structurally — not
    // only chromatically — distinct from an undetermined read.
    expect(unsetRow?.querySelector(`[data-setting-key="${SETTING_KEY}"]`)).toBeTruthy();
    expect(screen.queryByText(/budget.*unsupported/i)).toBeNull();

    // Unknown budget never renders as a pass.
    expect(screen.getAllByText('budget could not be determined').length).toBe(2);

    // A live size read does not manufacture a growth baseline. Every store is
    // explicit about the missing execution-owned history.
    expect(screen.getAllByText('growth could not be determined').length).toBe(5);
    expect(screen.getByText(/telemetry could not be determined for this store/)).toBeTruthy();
  });

  it('projects the active code-index generation as a compact pipeline card', async () => {
    stubTelemetry(telemetryPayload(), emptyStorageFindingsPayload(), codeIndexFreshnessEnvelope());
    renderObservatory();

    await screen.findByText('generation.catchup.01');
    const pipeline = screen.getByLabelText('Code-index pipeline');
    const text = pipeline.textContent ?? '';
    expect(text).toContain('bulk commit · 50.0%');
    expect(text).toContain('generation.catchup.01');
    expect(text).toContain('250 / 500 files');
    expect(text).toContain('250 files/s · 16.0 MiB lexical bytes/s');
    expect(text).toContain('elapsed 2m');
    expect(text).toContain('last commit 240ms');
  });

  it('keeps polling ready progress until the freshness envelope is ready', async () => {
    vi.useFakeTimers();
    const building = codeIndexFreshnessEnvelope();
    const buildingWorktree = building.payload.worktrees[0]!;
    const readyProgress = {
      ...buildingWorktree.progress,
      phase: 'ready',
      completed_files: 500,
      completed_lexical_bytes: 64 * 1024 * 1024,
      estimated_remaining_seconds: 0,
    };
    const transitioning = {
      ...building,
      domain_state: 'partial',
      payload: {
        ...building.payload,
        worktrees: [
          {
            ...buildingWorktree,
            latest_generation_id: readyProgress.generation_id,
            staleness_state: 'stale',
            coverage: 'partial',
            progress: readyProgress,
          },
        ],
      },
    };
    const authoritativeReady = readyCodeIndexFreshnessEnvelope();
    const ready = {
      ...authoritativeReady,
      payload: {
        ...authoritativeReady.payload,
        worktrees: [
          {
            ...authoritativeReady.payload.worktrees[0]!,
            latest_generation_id: readyProgress.generation_id,
            progress: readyProgress,
          },
        ],
      },
    };
    const progressResponses = [transitioning, ready, ready];
    let progressResponse = 0;
    stubTelemetry(
      telemetryPayload(),
      emptyStorageFindingsPayload(),
      () => progressResponses[Math.min(progressResponse++, progressResponses.length - 1)]!,
    );
    renderObservatory();

    await advanceTimers(0);
    expect(screen.getByText('ready · 100.0%')).toBeTruthy();
    expect(progressResponse).toBe(1);
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.getByText('ready · 100.0%')).toBeTruthy();
    expect(progressResponse).toBe(2);
    await advanceTimers(29_998);
    expect(progressResponse).toBe(2);
    await advanceTimers(1);
    await advanceTimers(0);
    expect(progressResponse).toBe(3);
  });

  it('accepts a later generation with a restarted epoch and rejects its delayed predecessor', async () => {
    vi.useFakeTimers();
    const first = codeIndexFreshnessEnvelope();
    const firstWorktree = first.payload.worktrees[0]!;
    const firstProgress = firstWorktree.progress;
    const replacement = {
      ...first,
      payload: {
        ...first.payload,
        worktrees: [
          {
            ...firstWorktree,
            progress: {
              ...firstProgress,
              generation_id: 'generation.catchup.02',
              daemon_incarnation: 2,
              producer_incarnation: 1,
              progress_epoch: 0,
              last_progress_micros: SAMPLE_CURRENT_MICROS - 1,
              completed_files: 1,
            },
          },
        ],
      },
    };
    const progressResponses = [first, replacement, first];
    let progressResponse = 0;
    stubTelemetry(
      telemetryPayload(),
      emptyStorageFindingsPayload(),
      () => progressResponses[Math.min(progressResponse++, progressResponses.length - 1)]!,
    );
    renderObservatory();

    await advanceTimers(0);
    expect(screen.getByText('generation.catchup.01')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.getByText('generation.catchup.02')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.queryByText('generation.catchup.01')).toBeNull();
  });

  it('accepts a same-generation publication after restart and rejects a delayed pre-restart epoch', async () => {
    vi.useFakeTimers();
    const beforeRestart = codeIndexFreshnessEnvelope();
    const beforeRestartWorktree = beforeRestart.payload.worktrees[0]!;
    const beforeRestartProgress = beforeRestartWorktree.progress;
    const afterRestart = {
      ...beforeRestart,
      payload: {
        ...beforeRestart.payload,
        worktrees: [
          {
            ...beforeRestartWorktree,
            progress: {
              ...beforeRestartProgress,
              daemon_incarnation: 2,
              producer_incarnation: 1,
              progress_epoch: 0,
              last_progress_micros: SAMPLE_CURRENT_MICROS - 1,
              completed_files: 1,
            },
          },
        ],
      },
    };
    const beforeRestartWithEpoch = {
      ...beforeRestart,
      payload: {
        ...beforeRestart.payload,
        worktrees: [
          {
            ...beforeRestartWorktree,
            progress: { ...beforeRestartProgress, progress_epoch: 8 },
          },
        ],
      },
    };
    const progressResponses = [beforeRestartWithEpoch, afterRestart, beforeRestartWithEpoch];
    let progressResponse = 0;
    stubTelemetry(
      telemetryPayload(),
      emptyStorageFindingsPayload(),
      () => progressResponses[Math.min(progressResponse++, progressResponses.length - 1)]!,
    );
    renderObservatory();

    await advanceTimers(0);
    expect(screen.getByText('250 / 500 files')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.getByText('1 / 500 files')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.queryByText('250 / 500 files')).toBeNull();
  });

  it('accepts a same-daemon remounted producer and rejects its retired predecessor', async () => {
    vi.useFakeTimers();
    const retired = codeIndexFreshnessEnvelope();
    const retiredWorktree = retired.payload.worktrees[0]!;
    const retiredProgress = retiredWorktree.progress;
    const retiredWithEpoch = {
      ...retired,
      payload: {
        ...retired.payload,
        worktrees: [
          {
            ...retiredWorktree,
            progress: { ...retiredProgress, progress_epoch: 100 },
          },
        ],
      },
    };
    const remounted = {
      ...retired,
      payload: {
        ...retired.payload,
        worktrees: [
          {
            ...retiredWorktree,
            progress: {
              ...retiredProgress,
              producer_incarnation: 2,
              progress_epoch: 2,
              completed_files: 1,
            },
          },
        ],
      },
    };
    const progressResponses = [retiredWithEpoch, remounted, retiredWithEpoch];
    let progressResponse = 0;
    stubTelemetry(
      telemetryPayload(),
      emptyStorageFindingsPayload(),
      () => progressResponses[Math.min(progressResponse++, progressResponses.length - 1)]!,
    );
    renderObservatory();

    await advanceTimers(0);
    expect(screen.getByText('250 / 500 files')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.getByText('1 / 500 files')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.queryByText('250 / 500 files')).toBeNull();
  });

  it('removes an unmounted worktree before switching to the next scope pipeline', async () => {
    vi.useFakeTimers();
    const first = codeIndexFreshnessEnvelope();
    const firstWorktree = first.payload.worktrees[0]!;
    const firstProgress = firstWorktree.progress;
    const aggregate = {
      ...first,
      payload: {
        ...first.payload,
        worktrees: [
          {
            ...firstWorktree,
            worktree_root: '/worktrees/aggregate-alpha',
            progress: { ...firstProgress, generation_id: 'generation.scope.alpha' },
          },
        ],
      },
    };
    const unmounted = {
      ...aggregate,
      payload: { ...aggregate.payload, worktrees: [] },
    };
    const projectBeta = {
      ...aggregate,
      scope: { project_id: 'project.beta', storage_mode: 'project', store_root: '/stores/beta' },
      payload: {
        ...aggregate.payload,
        worktrees: [
          {
            ...firstWorktree,
            worktree_root: '/worktrees/project-beta',
            progress: { ...firstProgress, generation_id: 'generation.scope.beta' },
          },
        ],
      },
    };
    let aggregateResponse = 0;
    stubTelemetry(telemetryPayload(), emptyStorageFindingsPayload(), (url: string) => {
      if (url.startsWith('/api/projects/project.beta/')) return projectBeta;
      return [aggregate, unmounted][Math.min(aggregateResponse++, 1)]!;
    });
    renderObservatory();

    await advanceTimers(0);
    expect(screen.getByText('generation.scope.alpha')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.queryByText('generation.scope.alpha')).toBeNull();

    act(() => useScope.getState().selectProject('project.beta', 'Project Beta', 'selected'));
    await advanceTimers(0);
    expect(screen.getByText('generation.scope.beta')).toBeTruthy();
    expect(screen.queryByText('generation.scope.alpha')).toBeNull();
    expect(document.querySelectorAll('[data-code-index-generation]').length).toBe(1);
  });

  it('clears an active scope pipeline before accepting a direct active scope replacement', async () => {
    vi.useFakeTimers();
    const first = codeIndexFreshnessEnvelope();
    const firstWorktree = first.payload.worktrees[0]!;
    const firstProgress = firstWorktree.progress;
    const projectAlpha = {
      ...first,
      scope: { project_id: 'project.alpha', storage_mode: 'project', store_root: '/stores/alpha' },
      payload: {
        ...first.payload,
        worktrees: [
          {
            ...firstWorktree,
            worktree_root: '/worktrees/project-alpha',
            progress: { ...firstProgress, generation_id: 'generation.scope.alpha' },
          },
        ],
      },
    };
    const projectBeta = {
      ...projectAlpha,
      scope: { project_id: 'project.beta', storage_mode: 'project', store_root: '/stores/beta' },
      payload: {
        ...projectAlpha.payload,
        worktrees: [
          {
            ...firstWorktree,
            worktree_root: '/worktrees/project-beta',
            progress: { ...firstProgress, generation_id: 'generation.scope.beta' },
          },
        ],
      },
    };
    stubTelemetry(telemetryPayload(), emptyStorageFindingsPayload(), (url: string) =>
      url.startsWith('/api/projects/project.alpha/') ? projectAlpha : projectBeta,
    );
    act(() => useScope.getState().selectProject('project.alpha', 'Project Alpha', 'active'));
    renderObservatory();

    await advanceTimers(0);
    expect(screen.getByText('generation.scope.alpha')).toBeTruthy();
    act(() => useScope.getState().selectProject('project.beta', 'Project Beta', 'active'));
    await advanceTimers(0);
    expect(screen.getByText('generation.scope.beta')).toBeTruthy();
    expect(screen.queryByText('generation.scope.alpha')).toBeNull();
    expect(document.querySelectorAll('[data-code-index-generation]').length).toBe(1);
  });

  it('distinguishes an unset budget from an undetermined one in the rendered state', async () => {
    stubTelemetry(telemetryPayload());
    renderObservatory();

    await screen.findByText('graph · memory (shared store file)');
    const budgets = Array.from(
      document.querySelectorAll('[data-dimension="budget"]'),
    ).map((node) => ({
      state: node.getAttribute('data-dimension-state'),
      tone: node.getAttribute('data-dimension-tone'),
    }));
    expect(budgets.map((row) => row.state)).toEqual([
      'within_budget',
      'over_budget',
      'unset',
      'unknown',
      'unknown',
    ]);
    const unset = budgets.find((row) => row.state === 'unset');
    const unknown = budgets.find((row) => row.state === 'unknown');
    expect(unset?.tone).toBe('unset');
    expect(unknown?.tone).toBe('unknown');
    expect(unset?.tone).not.toBe(unknown?.tone);
  });

  it('renders table-growth unavailable states distinctly without zero measurements', async () => {
    stubTelemetry(telemetryPayload());
    renderObservatory();

    await screen.findByText('graph · memory (shared store file)');
    for (const [state, label] of [
      ['unknown', 'Unknown'],
      ['denied', 'Denied'],
      ['unsupported', 'Unsupported'],
    ] as const) {
      const panel = document.querySelector(`[data-table-growth-state="${state}"]`);
      expect(panel).toBeTruthy();
      expect(panel?.textContent).toContain(label);
      expect(panel?.querySelector('[data-table-growth-sample]')).toBeNull();
      expect(panel?.textContent).not.toContain('+0 B');
    }
  });

  it('renders an observed significant table row with its measured bytes and window', async () => {
    stubTelemetry(telemetryPayload());
    renderObservatory();

    await screen.findByText('graph · memory (shared store file)');
    const row = document.querySelector('[data-table-growth-sample="messages"]');
    expect(row).toBeTruthy();
    // The whole point of the observed state: a real delta, the byte window it
    // was measured over, and the two watermarks it spans. If any of it stops
    // rendering, observed growth has silently disappeared.
    expect(row?.textContent).toContain('+1.0 MiB');
    expect(row?.textContent).toContain('10.0 MiB → 11.0 MiB');
    expect(row?.textContent).toContain(`${SAMPLE_PREVIOUS_ISO} → ${SAMPLE_CURRENT_ISO}`);
    expect(row?.textContent).not.toContain('NaN');
    expect(row?.textContent).not.toContain('Invalid Date');
  });

  it('keeps an observed read with baseline-pending tables visibly partial', async () => {
    stubTelemetry(telemetryPayload());
    renderObservatory();

    await screen.findByText('graph · memory (shared store file)');
    const panel = document.querySelector('[data-table-growth-state="observed"]');
    expect(panel).toBeTruthy();
    expect(panel?.textContent).toContain('partial table coverage');
    expect(panel?.textContent).toContain('2 of 3 current_tables compared');

    // A table with no previous watermark reports its current size and says the
    // baseline is pending — it never reports a delta, not even zero.
    const pending = panel?.querySelector('[data-table-growth-omission="baseline_pending"]');
    expect(pending?.textContent).toContain('embeddings');
    expect(pending?.textContent).toContain('4.0 MiB now');
    expect(pending?.textContent).toContain('no previous watermark');
    expect(pending?.textContent).not.toContain('+0 B');
    expect(
      screen.getByText('embeddings: no previous table watermark exists; baseline pending'),
    ).toBeTruthy();

    // Below-threshold tables keep their measured window, formatted in the same
    // units as the significant rows, and stay informational.
    const below = panel?.querySelector('[data-table-growth-omission="below_threshold"]');
    expect(below?.textContent).toContain('+512.0 KiB');
    expect(below?.textContent).toContain('100.0 MiB → 100.5 MiB');
    expect(panel?.getAttribute('data-table-growth-tone')).toBe('ready');
  });

  it('states table-growth coverage across all stores separately from each store', async () => {
    stubTelemetry(telemetryPayload());
    renderObservatory();

    const fleet = await screen.findByLabelText('Table growth coverage across all stores');
    expect(fleet.getAttribute('data-table-growth-coverage')).toBe('partial');
    expect(fleet.textContent).toContain('1 of 5 store_table_growth_reads fully compared');
    for (const reason of [
      'lcm.db: denied',
      'savings.db: no baseline yet',
      'sessions.db: unsupported',
      'incident.db: unavailable',
    ]) {
      expect(fleet.textContent).toContain(reason);
    }
    // Aggregate scope and per-store scope are separate regions, so a partial
    // fleet cannot hide behind one healthy-looking store card.
    expect(fleet.querySelector('[data-table-growth-state]')).toBeNull();
    expect(
      document.querySelector('[data-table-growth-state="observed"]')?.textContent,
    ).toContain('Coverage · this store');
  });

  it('gives every per-store table-growth region a distinct accessible name', async () => {
    stubTelemetry(telemetryPayload());
    renderObservatory();

    await screen.findByText('graph · memory (shared store file)');
    const labels = Array.from(document.querySelectorAll('[data-table-growth-state]')).map(
      (region) => region.getAttribute('aria-label'),
    );
    expect(labels.length).toBeGreaterThan(1);
    expect(new Set(labels).size).toBe(labels.length);
    expect(labels).toContain('Per-table growth · graph.db');
  });

  it('renders no baseline yet and surfaces every table omission reason', async () => {
    stubTelemetry(telemetryPayload());
    renderObservatory();

    expect((await screen.findAllByText(/no baseline yet/i)).length).toBeGreaterThan(0);
    expect(screen.queryByText(/no growth/i)).toBeNull();
    expect(
      screen.getByText(/observed growth was below the informational significance threshold/i),
    ).toBeTruthy();
  });

  it('gives a byte-only store its size and an unknown free-page reading', async () => {
    stubTelemetry({ ...telemetryPayload(), stores: [byteOnlyStore()] });
    renderObservatory();

    // The size is a real measurement and is printed as one.
    expect(await screen.findByText('40.0 MiB')).toBeTruthy();
    // The capacity bar is drawn for this store rather than withheld, and says
    // what it does not know instead of filling to 100% at "0.0% free pages".
    expect(screen.getByText('free pages unknown')).toBeTruthy();
    expect(
      screen.getByText(/no page-level sample, so free pages are unmeasured rather than zero/),
    ).toBeTruthy();
    // Nothing may announce a measured free-page share for pages nobody sampled.
    expect(screen.queryByRole('img', { name: /free pages/ })).toBeNull();
    expect(screen.queryByText(/0\.0%/)).toBeNull();
  });

  it('renders the canonical Doctor storage family with typed kinds and provenance', async () => {
    stubTelemetry(telemetryPayload(), storageFindingsPayload());
    renderObservatory();

    expect((await screen.findAllByText('Over-budget stores')).length).toBeGreaterThan(0);
    for (const label of [
      'Orphan stores',
      'Incident debris',
      'Retention backlog',
      'Table growth',
    ]) {
      expect(screen.getAllByText(label).length).toBeGreaterThan(0);
    }
    expect(document.querySelectorAll('[data-storage-finding-kind]')).toHaveLength(4);
    expect(
      document.querySelector('[data-storage-finding-kind="over_budget_store"]')?.textContent,
    ).toContain('Degraded');
    expect(screen.getByText('store size observed against soft budget')).toBeTruthy();
    expect(
      screen.getByText('storage.over_budget_store.sessions.db.observed-8388608b.overage-4194304b'),
    ).toBeTruthy();
    expect(screen.queryByText(/use-case\.application\.storage/)).toBeNull();
  });

  it('renders every finding producer source state without treating partial as clean', async () => {
    stubTelemetry(telemetryPayload(), sourceStatusFindingsPayload());
    renderObservatory();

    expect(await screen.findByLabelText('Storage finding source status')).toBeTruthy();
    const statusFor = (kind: string) =>
      document.querySelector(`[data-storage-source-kind="${kind}"]`);

    expect(statusFor('over_budget_store')?.getAttribute('data-storage-source-state')).toBe(
      'partial',
    );
    expect(statusFor('over_budget_store')?.textContent).toContain(
      'the canonical report did not carry per-producer completion evidence',
    );
    expect(statusFor('orphan_store')?.getAttribute('data-storage-source-state')).toBe('partial');
    expect(statusFor('incident_debris_present')?.getAttribute('data-storage-source-state')).toBe(
      'real',
    );
    expect(statusFor('retention_backlog')?.getAttribute('data-storage-source-state')).toBe(
      'partial',
    );
    expect(screen.queryByText(/all storage checks clean/i)).toBeNull();
  });
});

function renderObservatory() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  // The wing camera lives in the address (`?wing=`), so the page needs a
  // router to read it. Every read this file asserts on sits on the default
  // Diagnosis wing.
  return render(
    <MemoryRouter initialEntries={['/observatory']}>
      <QueryClientProvider client={client}>
        <ObservatoryPage />
      </QueryClientProvider>
    </MemoryRouter>,
  );
}

async function advanceTimers(milliseconds: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(milliseconds);
  });
}

function stubTelemetry(
  payload: unknown,
  findingsPayload: unknown = emptyStorageFindingsPayload(),
  codeIndexFreshnessPayload: unknown | ((url: string) => unknown) = readyCodeIndexFreshnessEnvelope(),
) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const route = url.replace(/^\/api\/projects\/[^/]+/, '/api');
      if (route === '/api/storage/telemetry') return jsonResponse(envelope(payload));
      if (route === '/api/storage/findings') {
        return jsonResponse(envelope(findingsPayload));
      }
      if (route === '/api/code-index/freshness') {
        return jsonResponse(
          typeof codeIndexFreshnessPayload === 'function'
            ? codeIndexFreshnessPayload(url)
            : codeIndexFreshnessPayload,
        );
      }
      if (route === '/api/doctor/findings') {
        return jsonResponse(
          envelope({
            family_filter: null,
            entries: [],
            report_coverage: null,
            known_families: ['storage'],
            note: 'no admitted Doctor report source is available for this dashboard scope',
          }),
        );
      }
      throw new Error(`unexpected fetch ${url}`);
    }),
  );
}

function codeIndexFreshnessEnvelope() {
  return {
    ...readyCodeIndexFreshnessEnvelope(),
    domain_state: 'loading',
    coverage: {
      completeness: 'unknown',
      eligible: null,
      examined: null,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: null,
      unit: 'mounted_worktree',
      omission_reasons: [],
    },
    payload: {
      worktrees: [
        {
          ...codeIndexWorktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
          progress: {
            generation_id: 'generation.catchup.01',
            daemon_incarnation: 1,
            producer_incarnation: 1,
            progress_epoch: 1,
            sealed_source_digest: 'sha256:sealed-source-catchup',
            phase: 'bulk_commit',
            committed_pages: 16,
            committed_chunks: 10_000,
            committed_imports: 480,
            committed_payload_bytes: 16 * 1024 * 1024,
            completed_files: 250,
            total_files: 500,
            completed_lexical_bytes: 32 * 1024 * 1024,
            total_lexical_bytes: 64 * 1024 * 1024,
            current_batch_pages: 4,
            current_batch_payload_bytes: 4 * 1024 * 1024,
            elapsed_micros: 120_000_000,
            last_commit_latency_micros: 240_000,
            files_per_second: 250,
            lexical_bytes_per_second: 16 * 1024 * 1024,
            estimated_remaining_seconds: 120,
            last_progress_micros: SAMPLE_CURRENT_MICROS,
            blocked_reason: null,
          },
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    },
  };
}

function readyCodeIndexFreshnessEnvelope() {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: SAMPLE_CURRENT_MICROS },
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
    freshness: { state: 'fresh', observed_at_micros: SAMPLE_CURRENT_MICROS, watermark: null },
    domain_state: 'ready',
    legal_actions: [
      { kind: 'refresh', operation: 'use-case.dashboard.code-index.freshness.refresh' },
    ],
    payload: {
      worktrees: [{ ...codeIndexWorktree(), progress: null }],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    },
  };
}

function codeIndexWorktree() {
  return {
    worktree_root: '/fast/projects/tracedecay',
    repository_id: 'repository.tracedecay',
    worktree_id: 'worktree.primary',
    source_reference: 'refs/heads/main',
    source_revision: null,
    latest_generation_id: 'generation.2f8c41ab',
    snapshot_content_identity: 'sha256:9c1f4a2e7b05',
    sealed_at_micros: SAMPLE_CURRENT_MICROS - 214_000_000,
    last_reconcile_micros: SAMPLE_CURRENT_MICROS - 8_400_000,
    staleness_state: 'fresh',
    rebuild_in_flight: false,
    hook_hint_count: 0,
    coverage: 'complete',
    parked: null,
  };
}

function emptyStorageFindingsPayload() {
  return {
    family_filter: 'storage',
    entries: [],
    report_coverage: null,
    known_families: ['storage'],
    note: 'canonical Doctor storage family contained no entries',
    kind_statuses: sourceStatuses(),
  };
}

function storageFindingsPayload() {
  const entry = (
    storageKind:
      | 'over_budget_store'
      | 'orphan_store'
      | 'incident_debris_present'
      | 'retention_backlog',
    state: 'degraded' | 'stale',
    reference: string,
    statement: string,
  ) => ({
    finding: {
      family: 'storage',
      state,
      evidence: [{ family: 'storage', reference }],
      coverage: { completeness: 'complete', statement },
    },
    storage_kind: storageKind,
  });
  return {
    family_filter: 'storage',
    entries: [
      entry(
        'over_budget_store',
        'degraded',
        'storage.over_budget_store.sessions.db.observed-8388608b.overage-4194304b',
        'store size observed against soft budget',
      ),
      entry(
        'orphan_store',
        'degraded',
        'storage.orphan_store.orphan.db.age-86400000000us.size-1048576b',
        'store identity no longer resolves to a live repository root',
      ),
      entry(
        'incident_debris_present',
        'degraded',
        'storage.incident_debris_present.graph.db.count-2.bytes-3145728b',
        'quarantine-eligible incident artifacts present beside a live store',
      ),
      entry(
        'retention_backlog',
        'stale',
        'storage.retention_backlog.sessions.db.table-lcm_raw_messages.bytes-4194304b',
        'retention-eligible rows are past their window awaiting collection',
      ),
    ],
    report_coverage: {
      families: [{ family: 'storage', consultation: { status: 'consulted' } }],
      completeness: 'complete',
      statement: {
        completeness: 'complete',
        statement: 'storage retention and size authorities were consulted',
      },
    },
    known_families: [
      'advisory',
      'configuration',
      'storage_runtime',
      'storage',
      'language_server',
      'semantic_index',
      'observability',
    ],
    note: 'storage retention and size authorities were consulted',
    kind_statuses: sourceStatuses({
      over_budget_store: {
        state: 'partial',
        observed_entries: 2,
        reason: '2 stores evaluated; 1 unset; 2 undetermined',
      },
      orphan_store: {
        state: 'real',
        observed_entries: 1,
        reason: 'canonical Doctor producer returned observed evidence',
      },
      incident_debris_present: {
        state: 'real',
        observed_entries: 1,
        reason: 'canonical Doctor producer returned observed evidence',
      },
      retention_backlog: {
        state: 'real',
        observed_entries: 1,
        reason: 'canonical Doctor producer returned observed evidence',
      },
    }),
  };
}

function sourceStatusFindingsPayload() {
  return {
    ...emptyStorageFindingsPayload(),
    kind_statuses: sourceStatuses({
      over_budget_store: {
        state: 'partial',
        observed_entries: 0,
        reason: 'the canonical report did not carry per-producer completion evidence',
      },
      orphan_store: {
        state: 'partial',
        observed_entries: 0,
        reason: 'the canonical report did not carry per-producer completion evidence',
      },
      incident_debris_present: {
        state: 'real',
        observed_entries: 1,
        reason: 'canonical Doctor producer returned observed evidence',
      },
      retention_backlog: {
        state: 'partial',
        observed_entries: 0,
        reason: 'retention watermark was stale',
      },
    }),
  };
}

type SourceStatus = {
  state: 'real' | 'unset' | 'partial' | 'unsupported';
  observed_entries: number;
  reason: string;
};

function sourceStatuses(overrides: Partial<Record<string, SourceStatus>> = {}) {
  return [
    'over_budget_store',
    'orphan_store',
    'incident_debris_present',
    'retention_backlog',
    'table_growth',
  ].map((kind) => ({
    kind,
    ...(overrides[kind] ?? {
      state: 'unsupported',
      observed_entries: 0,
      reason: 'no admitted Doctor report source is available for this dashboard scope',
    }),
  }));
}

function telemetryPayload() {
  const tableGrowth = [
    {
      state: 'observed',
      // Two of the three current tables had a previous watermark to compare
      // against, so this observed read is deliberately partial.
      coverage: partialCoverage(3, 2, 'current_tables', [
        'embeddings: no previous table watermark exists; baseline pending',
      ]),
      significant_samples: [
        {
          table: 'messages',
          previous_bytes: 10_485_760,
          current_bytes: 11_534_336,
          growth_bytes: 1_048_576,
          previous_observed_at: SAMPLE_PREVIOUS_MICROS,
          current_observed_at: SAMPLE_CURRENT_MICROS,
        },
      ],
      omissions: [
        {
          kind: 'below_threshold',
          table: 'metadata',
          previous_bytes: 104_857_600,
          current_bytes: 105_381_888,
          growth_bytes: 524_288,
          previous_observed_at: SAMPLE_PREVIOUS_MICROS,
          current_observed_at: SAMPLE_CURRENT_MICROS,
          reason: 'observed growth was below the informational significance threshold',
        },
        {
          kind: 'baseline_pending',
          table: 'embeddings',
          current_bytes: 4_194_304,
          observed_at: SAMPLE_CURRENT_MICROS,
          reason: 'embeddings: no previous table watermark exists; baseline pending',
        },
      ],
      omission_reasons: [
        'metadata: observed growth was below the informational significance threshold',
        'embeddings: no previous table watermark exists; baseline pending',
      ],
    },
    {
      state: 'denied',
      coverage: partialTableGrowthCoverage('per-table payload growth measurement was denied'),
      omission_reasons: ['per-table payload growth measurement was denied for this store'],
    },
    {
      state: 'baseline_established',
      coverage: partialTableGrowthCoverage('no baseline yet'),
      observed_at: 100,
      tables_observed: 7,
      omission_reasons: [
        'no baseline yet; this read established the first per-table payload watermark',
      ],
    },
    {
      state: 'unsupported',
      coverage: partialTableGrowthCoverage(
        'per-table payload growth measurement is unsupported',
      ),
      omission_reasons: ['per-table payload growth measurement is unsupported for this store'],
    },
    {
      state: 'unknown',
      coverage: partialTableGrowthCoverage(
        'per-table payload growth measurement is unavailable',
      ),
      omission_reasons: ['per-table payload growth measurement is unavailable for this store'],
    },
  ];
  let tableGrowthIndex = 0;
  return {
    stores: [
      {
        store: 'graph.db',
        role: 'graph',
        roles: ['graph', 'memory'],
        path: '/project/.tracedecay/graph.db',
        read: {
          kind: 'observed',
          sample: {
            store: 'graph.db',
            page_size_bytes: 4096,
            page_count: 52_400,
            freelist_pages: 1_280,
            observed_at: 100,
          },
        },
        total_bytes: 214_630_400,
        free_bytes: 5_242_880,
        free_page_ratio: 0.024,
        budget: {
          state: 'evaluated',
          evaluation: { state: 'within_budget', observed: 214_630_400, soft_limit: 536_870_912 },
          setting_key: SETTING_KEY,
          reason: 'evaluated against the owner-configured soft limit of 536870912 bytes',
        },
        growth: {
          state: 'unknown',
          reason: 'no execution-owned store-size watermark is available',
        },
      },
      {
        store: 'lcm.db',
        role: 'lcm',
        roles: ['lcm'],
        path: '/profile/lcm.db',
        read: {
          kind: 'observed',
          sample: {
            store: 'lcm.db',
            page_size_bytes: 4096,
            page_count: 180_224,
            freelist_pages: 2_048,
            observed_at: 100,
          },
        },
        total_bytes: 738_197_504,
        free_bytes: 8_388_608,
        free_page_ratio: 0.011,
        budget: {
          state: 'evaluated',
          evaluation: {
            state: 'over_budget',
            observed: 738_197_504,
            soft_limit: 536_870_912,
            overage: 201_326_592,
          },
          setting_key: SETTING_KEY,
          reason: 'evaluated against the owner-configured soft limit of 536870912 bytes',
        },
        growth: {
          state: 'unknown',
          reason: 'no execution-owned store-size watermark is available',
        },
      },
      {
        store: 'savings.db',
        role: 'savings',
        roles: ['savings'],
        path: '/profile/savings.db',
        read: {
          kind: 'observed',
          sample: {
            store: 'savings.db',
            page_size_bytes: 4096,
            page_count: 18_200,
            freelist_pages: 420,
            observed_at: 100,
          },
        },
        total_bytes: 74_547_200,
        free_bytes: 1_720_320,
        free_page_ratio: 0.023,
        budget: {
          state: 'unset',
          reason: 'no soft size budget is configured by the owner for this store',
          setting_key: SETTING_KEY,
        },
        growth: {
          state: 'unknown',
          reason: 'no execution-owned store-size watermark is available',
        },
      },
      {
        store: 'sessions.db',
        role: 'sessions',
        roles: ['sessions'],
        path: '/profile/sessions.db',
        read: {
          kind: 'observed',
          sample: {
            store: 'sessions.db',
            page_size_bytes: 4096,
            page_count: 9_600,
            freelist_pages: 96,
            observed_at: 100,
          },
        },
        total_bytes: 39_321_600,
        free_bytes: 393_216,
        free_page_ratio: 0.01,
        budget: {
          state: 'unknown',
          reason: 'the resolved runtime configuration could not be read',
        },
        growth: {
          state: 'unknown',
          reason: 'no execution-owned store-size watermark is available',
        },
      },
      {
        store: 'incident.db',
        role: 'incident',
        roles: ['incident'],
        path: '/profile/incident.db',
        read: { kind: 'unknown', store: 'incident.db' },
        total_bytes: null,
        free_bytes: null,
        free_page_ratio: null,
        budget: {
          state: 'unknown',
          reason: 'no observed size sample, so a configured budget could not be evaluated',
        },
        growth: {
          state: 'unknown',
          reason: 'no execution-owned store-size watermark is available',
        },
      },
    ].map((store) => ({ ...store, table_growth: tableGrowth[tableGrowthIndex++] })),
    budget_note: 'budgets are owner configuration: sync.retention.v1 store_soft_budgets_bytes',
    growth_note: 'growth is measured over the watermarks this daemon has recorded since it started',
    table_growth_threshold: {
      absolute_bytes: 67_108_864,
      relative_floor_bytes: 1_048_576,
      relative_percent: 10,
    },
    table_growth_coverage: {
      completeness: 'partial',
      eligible: 5,
      examined: 1,
      matched: null,
      excluded: null,
      omitted: 4,
      unknown: null,
      denominator: 5,
      unit: 'store_table_growth_reads',
      omission_reasons: [
        'lcm.db: denied',
        'savings.db: no baseline yet',
        'sessions.db: unsupported',
        'incident.db: unavailable',
      ],
    },
  };
}

/** A store whose size read produced a byte total with no page-level sample —
 * the one read kind that has a real size and no free-page figure at all. */
function byteOnlyStore() {
  return {
    store: 'savings.db',
    role: 'savings',
    roles: ['savings'],
    path: '/profile/savings.db',
    read: {
      kind: 'observed_bytes',
      store: 'savings.db',
      total_bytes: 41_943_040,
      observed_at: SAMPLE_CURRENT_MICROS,
    },
    total_bytes: 41_943_040,
    free_bytes: null,
    free_page_ratio: null,
    budget: {
      state: 'unknown',
      reason: 'no page sample, so a configured budget could not be evaluated',
    },
    growth: {
      state: 'unknown',
      reason: 'no execution-owned store-size watermark is available',
    },
    table_growth: {
      state: 'unknown',
      coverage: partialTableGrowthCoverage(
        'per-table payload growth measurement is unavailable',
      ),
      omission_reasons: ['per-table payload growth measurement is unavailable for this store'],
    },
  };
}

function partialCoverage(
  denominator: number,
  examined: number,
  unit: string,
  omissionReasons: string[],
) {
  return {
    completeness: 'partial',
    eligible: denominator,
    examined,
    matched: null,
    excluded: null,
    omitted: denominator - examined,
    unknown: null,
    denominator,
    unit,
    omission_reasons: omissionReasons,
  };
}

function partialTableGrowthCoverage(reason: string) {
  return {
    completeness: 'partial',
    eligible: 1,
    examined: 0,
    matched: null,
    excluded: null,
    omitted: 1,
    unknown: null,
    denominator: 1,
    unit: 'store_table_growth_reads',
    omission_reasons: [reason],
  };
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function envelope<T>(payload: T) {
  return {
    schema_revision: 1,
    scope: { project_id: 'project.observatory', storage_mode: 'project_local', store_root: '/p' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 100 },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'partial',
      eligible: 5,
      examined: 4,
      matched: null,
      excluded: null,
      omitted: 1,
      unknown: null,
      denominator: 5,
      unit: 'stores',
      omission_reasons: ['store telemetry read failed (pragma unavailable)'],
    },
    freshness: { state: 'fresh', observed_at_micros: 100, watermark: null },
    domain_state: 'ready',
    legal_actions: [
      { kind: 'refresh', operation: 'use-case.dashboard.storage.telemetry.refresh' },
    ],
    payload,
  };
}

describe('ObservatoryPage duplicate finding identities', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  /**
   * The live regression: a real report can carry two findings of one kind
   * whose first evidence names the same reference (observed live: repeated
   * retention_backlog rows for one store). Both must render as cards, under
   * unique React keys — the old `kind:reference` key collided and React
   * warned about two children with the same key.
   */
  it('renders same-kind same-reference findings as distinct cards with unique keys', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const repeated = (statement: string) => ({
      finding: {
        family: 'storage',
        state: 'stale',
        evidence: [
          { family: 'storage', reference: 'storage.retention_backlog.sessions.db.no-backlog' },
          // The same reference twice inside one finding, which the evidence
          // list must also key uniquely.
          { family: 'storage', reference: 'storage.retention_backlog.sessions.db.no-backlog' },
        ],
        coverage: { completeness: 'complete', statement },
      },
      storage_kind: 'retention_backlog',
    });
    stubTelemetry(telemetryPayload(), {
      ...emptyStorageFindingsPayload(),
      entries: [repeated('first reading'), repeated('second reading')],
      note: 'storage retention and size authorities were consulted',
    });
    renderObservatory();

    // One card per entry, told apart by their coverage statements — the label
    // text alone also appears in the source-status strip.
    expect(await screen.findByText('first reading')).toBeTruthy();
    expect(await screen.findByText('second reading')).toBeTruthy();
    expect(document.querySelectorAll('[data-storage-finding-kind="retention_backlog"]')).toHaveLength(
      2,
    );

    const duplicateKeyWarnings = consoleError.mock.calls.filter((call) =>
      String(call[0]).includes('two children with the same key'),
    );
    expect(duplicateKeyWarnings).toEqual([]);
    consoleError.mockRestore();
  });
});
