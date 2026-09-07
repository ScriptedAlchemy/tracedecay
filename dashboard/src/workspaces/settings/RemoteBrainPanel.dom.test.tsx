/**
 * Remote Brain operational plane, from the canonical status read.
 *
 * Four readings are pinned here. Observed-ready is the clean journey;
 * observed-with-quarantine/recovery must not look ready; unconfigured and
 * unavailable stay visually distinct and never invent zero spool counts.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { RemoteBrainPanel } from './RemoteBrainPanel.tsx';

function serve(body: unknown, status = 200) {
  return vi.fn(
    async () =>
      ({
        ok: status >= 200 && status < 300,
        status,
        json: async () => body,
      }) as Response,
  );
}

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={client}>
      <RemoteBrainPanel />
    </QueryClientProvider>,
  );
}

function envelope(payload: unknown, domainState = 'ready') {
  return {
    schema_revision: 1,
    scope: {
      project_id: 'tracedecay',
      storage_mode: 'project_local',
      store_root: '/fixture',
    },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 10 },
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
      unit: 'remote_operational_status',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: 10, watermark: null },
    domain_state: domainState,
    legal_actions: [{ kind: 'refresh', operation: 'use-case.dashboard.remote.status.refresh' }],
    payload,
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('the Remote Brain operational plane', () => {
  it('renders an observed ready reading with fence identities and no fake recovery', async () => {
    vi.stubGlobal(
      'fetch',
      serve(
        envelope({
          kind: 'observed',
          listener: 'serving',
          coverage: 'complete',
          readiness: 'ready',
          enrollment_configured: true,
          authority: {
            state: 'available',
            fence: {
              brain_id: 'brain.status',
              shard_id: 'shard.status',
              generation_id: 'generation.status',
              placement_revision: 1,
              authority_epoch: 4,
              authority_node_id: 'node.authority',
            },
          },
          spool: { pending_count: 0, quarantined_count: 0, has_sequence_gap: false },
          replay_coverage_complete: true,
          current_backup_verified: true,
          failover_in_progress: false,
          recovery_required: false,
          observed_at: 10,
        }),
      ),
    );
    renderPanel();

    const panel = await screen.findByLabelText('Remote Brain');
    await waitFor(() =>
      expect(panel.querySelector('[data-remote-brain="observed"]')).not.toBeNull(),
    );
    expect(panel.textContent).toContain('brain.status');
    expect(panel.textContent).toContain('node.authority');
    expect(panel.textContent).toContain('configured');
    expect(panel.textContent).toMatch(/not required/i);
    expect(panel.querySelector('[data-remote-readiness="ready"]')).not.toBeNull();
    expect(panel.textContent).not.toMatch(/sequence gap/i);
  });

  it('renders quarantined spool and recovery as a distinct observed state', async () => {
    vi.stubGlobal(
      'fetch',
      serve(
        envelope(
          {
            kind: 'observed',
            listener: 'degraded',
            coverage: 'partial',
            readiness: 'recovery_required',
            enrollment_configured: true,
            authority: {
              state: 'partial',
              fence: {
                brain_id: 'brain.status',
                shard_id: 'shard.status',
                generation_id: 'generation.status',
                placement_revision: 1,
                authority_epoch: 4,
                authority_node_id: 'node.authority',
              },
              missing: ['fence_unverified'],
            },
            spool: { pending_count: 3, quarantined_count: 2, has_sequence_gap: true },
            replay_coverage_complete: false,
            current_backup_verified: false,
            failover_in_progress: true,
            recovery_required: true,
            observed_at: 10,
          },
          'error',
        ),
      ),
    );
    renderPanel();

    const panel = await screen.findByLabelText('Remote Brain');
    await waitFor(() =>
      expect(panel.querySelector('[data-remote-readiness="recovery_required"]')).not.toBeNull(),
    );
    expect(panel.textContent).toContain('3');
    expect(panel.textContent).toContain('2');
    expect(panel.textContent).toMatch(/sequence gap/i);
    expect(panel.textContent).toMatch(/fence unverified/i);
    expect(panel.textContent).toMatch(/required/i);
    expect(panel.textContent).toMatch(/in progress/i);
    expect(panel.querySelector('[data-remote-brain="unconfigured"]')).toBeNull();
  });

  it('keeps unconfigured visually distinct and invents no spool counts', async () => {
    vi.stubGlobal(
      'fetch',
      serve(envelope({ kind: 'unconfigured' }, 'unknown')),
    );
    renderPanel();

    const panel = await screen.findByLabelText('Remote Brain');
    await waitFor(() =>
      expect(panel.querySelector('[data-remote-brain="unconfigured"]')).not.toBeNull(),
    );
    expect(panel.textContent).toMatch(/not enrolled/i);
    expect(panel.textContent).not.toMatch(/spool pending/i);
    expect(panel.querySelector('[data-remote-brain="observed"]')).toBeNull();
    expect(panel.querySelector('[data-remote-brain="unavailable"]')).toBeNull();
  });

  it('keeps unavailable visually distinct from unconfigured', async () => {
    vi.stubGlobal(
      'fetch',
      serve(
        envelope(
          {
            kind: 'unavailable',
            note: 'the dashboard is not attached to a daemon-owned remote operational status reader',
          },
          'unsupported',
        ),
      ),
    );
    renderPanel();

    const panel = await screen.findByLabelText('Remote Brain');
    await waitFor(() =>
      expect(panel.querySelector('[data-remote-brain="unavailable"]')).not.toBeNull(),
    );
    expect(panel.textContent).toMatch(/not attached/i);
    expect(panel.querySelector('[data-remote-brain="unconfigured"]')).toBeNull();
    expect(panel.textContent).not.toMatch(/spool pending/i);
  });
});
