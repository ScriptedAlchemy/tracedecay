import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import { useScope, type ScopeWritability } from '../../data/scope/store.ts';
import { SettingsPage, findConfigSection } from './SettingsPage.tsx';
import { applySettingsMutation } from './settingsMutation.ts';

/** The dashboard pointed at the project the daemon has active — the scope every
 * case below is about something other than. */
const ACTIVE_SCOPE: ScopeWritability = { state: 'writable', target: 'tracedecay' };

const MAX_FILE_SIZE = 'project.config.max_file_size';
const POLL_SECS = 'project.config.sync.auto_track_pr_poll_secs';
const WATCHER_DEBOUNCE = 'user.watcher_debounce';
const WORKERS = 'user.code_index_workers';

function projectPatchResponse(current: unknown) {
  return {
    application_outcome: {
      Effect: {
        receipt: {
          request_id: 'request.configuration.settings-fixture',
          idempotency_key: 'settings-fixture',
          outcome: 'completed',
        },
      },
    },
    current,
  };
}

/**
 * These suites assert the exact request sequence the settings write protocol
 * performs: read, re-read for the confirmation, patch, refresh. The page also
 * reads `/api/capabilities` and `/api/remote/status` for the inspector, which
 * are neither part of that protocol nor able to affect it — so the recorders
 * below keep only settings traffic.
 */
function isSettingsRoute(url: string): boolean {
  const pathname = new URL(url, 'http://localhost').pathname;
  return pathname === '/api/settings' || pathname.startsWith('/api/settings/');
}

/** The table row for one full key. Rows are `role="row"` with `data-key`. */
function row(key: string): HTMLElement {
  const found = document.querySelector<HTMLElement>(`[role="row"][data-key="${key}"]`);
  if (!found) throw new Error(`no row for ${key}`);
  return found;
}

async function findRow(key: string): Promise<HTMLElement> {
  return waitFor(() => row(key));
}

/** Select a row and wait for its review to open under it. */
async function openReview(user: ReturnType<typeof userEvent.setup>, key: string) {
  await user.click(await findRow(key));
  return waitFor(() => {
    const panel = document.querySelector<HTMLElement>(`[data-settings-review="${key}"]`);
    if (!panel) throw new Error(`no review open for ${key}`);
    return panel;
  });
}

const proposal = (key: string) => screen.getByLabelText(`Proposed value for ${key}`);
const reviewReadout = () => document.querySelector('[data-review]')?.getAttribute('data-review');

describe('SettingsPage effective configuration review', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    useScope.getState().selectAllProjects();
  });

  it('inspects a row on hover and focus without selecting it', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));
    renderSettings();

    const target = await findRow(MAX_FILE_SIZE);
    expect(document.querySelector('[data-inspected-key]')).toBeNull();
    fireEvent.pointerEnter(target);

    const inspection = await waitFor(() => {
      const found = document.querySelector<HTMLElement>('[data-inspected-key]');
      if (!found) throw new Error('nothing inspected');
      return found;
    });
    expect(inspection.dataset['inspectedKey']).toBe(MAX_FILE_SIZE);
    expect(within(inspection).getByText('1,048,576')).toBeTruthy();
    expect(within(inspection).getByText('reported on apply')).toBeTruthy();
    // Inspecting is not selecting: no review opened, the row is not pressed.
    expect(target.getAttribute('aria-selected')).toBe('false');
    expect(document.querySelector('[data-settings-review]')).toBeNull();

    // Focus is the keyboard's hover.
    act(() => row(POLL_SECS).focus());
    expect(document.querySelector('[data-inspected-key]')?.getAttribute('data-inspected-key')).toBe(
      POLL_SECS,
    );
  });

  it('states provenance exactly as far as the wire serves it', async () => {
    const envelope = settings();
    const environment = settingsBody(envelope)['environment'] as Record<string, unknown>;
    environment['variables'] = [
      {
        name: 'TRACEDECAY_DATA_DIR',
        active: true,
        value: '/srv/tracedecay',
        description: 'Pins the user-level TraceDecay data directory.',
      },
      { name: 'TRACEDECAY_ENABLE_GLOBAL_DB', active: false, value: null, description: 'd' },
    ];
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(envelope)));
    renderSettings();

    expect((await findRow(MAX_FILE_SIZE)).dataset['provenance']).toBe('unserved');
    expect(row('environment.variables.TRACEDECAY_DATA_DIR').dataset['provenance']).toBe('explicit');
    expect(row('environment.variables.TRACEDECAY_ENABLE_GLOBAL_DB').dataset['provenance']).toBe(
      'default',
    );
    expect(
      within(row('environment.variables.TRACEDECAY_DATA_DIR')).getByText('/srv/tracedecay'),
    ).toBeTruthy();
    // Every other key says `unserved`: the surface never infers a layer.
    const served = [...document.querySelectorAll('[role="row"][data-key]')].filter(
      (element) => element.getAttribute('data-provenance') !== 'unserved',
    );
    expect(served.map((element) => element.getAttribute('data-key'))).toEqual([
      'environment.variables.TRACEDECAY_DATA_DIR',
      'environment.variables.TRACEDECAY_ENABLE_GLOBAL_DB',
    ]);
    // Origin is the group's stated location, or a stated absence.
    expect(within(row(MAX_FILE_SIZE)).getByTitle('/fast/projects/tracedecay/.tracedecay/config.toml')).toBeTruthy();
    expect(within(row('storage.store_root')).getByText('origin not served')).toBeTruthy();
  });

  it('reviews and applies a project patch with the held revision', async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    let applied = false;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        const body = init?.body ? JSON.parse(String(init.body)) : null;
        if (isSettingsRoute(url)) calls.push({ url, method, body });
        if (url === '/api/settings' && method === 'GET') {
          return jsonResponse(applied ? updatedSettings('rev-43') : settings());
        }
        if (url === '/api/settings/project' && method === 'PATCH') {
          applied = true;
          const envelope = updatedSettings('rev-43');
          settingsBody(envelope)['resync_recommended'] = true;
          return jsonResponse(projectPatchResponse(envelope));
        }
        throw new Error(`unexpected request ${method} ${url}`);
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    const panel = await openReview(user, MAX_FILE_SIZE);
    expect(reviewReadout()).toBe('none');
    expect(within(panel).getByText('rev-42')).toBeTruthy();
    expect(within(panel).getByText('applies to the active project')).toBeTruthy();

    const input = proposal(MAX_FILE_SIZE);
    await user.clear(input);
    await user.type(input, '2097152');
    // The proposal is a proposal: the effective value stands beside it.
    expect(row(MAX_FILE_SIZE).dataset['provenance']).toBe('edited');
    expect(within(row(MAX_FILE_SIZE)).getByText('1,048,576')).toBeTruthy();
    expect(within(row(MAX_FILE_SIZE)).getByText('2,097,152')).toBeTruthy();
    expect(document.querySelector('[data-settings-validation]')?.getAttribute('data-settings-validation')).toBe('ready');
    expect(reviewReadout()).toBe('proposal');

    await user.click(screen.getByRole('button', { name: 'Review project change' }));
    expect(reviewReadout()).toBe('pending');
    expect(within(panel).getByText(/"max_file_size": 2097152/)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Apply project settings' }).hasAttribute('disabled')).toBe(true);
    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this change against configuration revision rev-42/,
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Apply project settings' }));

    expect(await screen.findByText('Project settings saved')).toBeTruthy();
    expect(screen.getByText('Resync recommended')).toBeTruthy();
    expect(reviewReadout()).toBe('applied');
    // The effective value is the read-back, and the proposal is gone.
    await waitFor(() => expect(row(MAX_FILE_SIZE).dataset['provenance']).toBe('unserved'));
    expect(within(row(MAX_FILE_SIZE)).getByText('2,097,152')).toBeTruthy();
    expect(within(panel).getByText('revision now rev-43')).toBeTruthy();
    // Focus returned to the edited row once the review resolved.
    expect(document.activeElement).toBe(row(MAX_FILE_SIZE));
    expect(calls.map(({ method, url }) => `${method} ${url}`)).toEqual([
      'GET /api/settings',
      'GET /api/settings',
      'PATCH /api/settings/project',
      'GET /api/settings',
    ]);
    expect(calls[2]?.body).toEqual({
      expected_revision_id: 'rev-42',
      idempotency_key: expect.any(String),
      max_file_size: 2_097_152,
    });
  });

  it('round trips an exact code-index worker selection through review, PATCH, and refresh', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_other',
        label: 'Other project',
        activation: 'selected',
      },
    });
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    let applied = false;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        const body = init?.body ? JSON.parse(String(init.body)) : null;
        if (url.includes('/settings')) calls.push({ url, method, body });
        if (url === '/api/projects/proj_other/settings' && method === 'GET') {
          return jsonResponse(applied ? updatedWorkerSettings('profile-worker-rev-8') : settings());
        }
        if (url === '/api/settings' && method === 'GET') {
          return jsonResponse(applied ? updatedWorkerSettings('profile-worker-rev-8') : settings());
        }
        if (url === '/api/settings/user/code-index-workers' && method === 'PATCH') {
          applied = true;
          return jsonResponse(updatedWorkerSettings('profile-worker-rev-8'));
        }
        throw new Error(`unexpected request ${method} ${url}`);
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    // The admitted plan is served as its own read-only keys.
    expect(within(await findRow('user.code_index_worker_status.effective_workers')).getByText('4')).toBeTruthy();
    expect(within(row('user.code_index_worker_status.limiting_reason')).getByText('automatic_all_cores')).toBeTruthy();
    expect(within(row(WORKERS)).getByText('automatic')).toBeTruthy();

    const panel = await openReview(user, WORKERS);
    expect(within(panel).getByText('daemon restart')).toBeTruthy();
    expect(within(panel).getByText('applies to your TraceDecay profile')).toBeTruthy();
    await user.click(screen.getByLabelText('Exact number of cores'));
    const workers = screen.getByLabelText('Code-index worker count');
    await user.clear(workers);
    await user.type(workers, '4');
    expect(within(row(WORKERS)).getByText('exact · 4 workers')).toBeTruthy();
    await user.click(screen.getByRole('button', { name: 'Review code-index worker change' }));
    expect(within(panel).getByText(/"mode": "exact"/)).toBeTruthy();
    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this change against configuration revision profile-worker-rev-7/,
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Apply code-index worker selection' }));

    expect(await screen.findByText('Code-index worker selection saved')).toBeTruthy();
    expect(screen.getByText('Restart recommended')).toBeTruthy();
    expect(calls).toEqual([
      { method: 'GET', url: '/api/projects/proj_other/settings', body: null },
      { method: 'GET', url: '/api/settings', body: null },
      {
        method: 'PATCH',
        url: '/api/settings/user/code-index-workers',
        body: {
          expected_revision_id: 'profile-worker-rev-7',
          idempotency_key: expect.stringMatching(/^idempotency\.dashboard-settings\./),
          code_index_workers: { mode: 'exact', workers: 4 },
        },
      },
      { method: 'GET', url: '/api/projects/proj_other/settings', body: null },
    ]);
  });

  it('states when TRACEDECAY_INDEX_WORKERS overrides the persisted worker selection', async () => {
    const overridden = settings();
    const user = settingsBody(overridden)['user'] as Record<string, unknown>;
    user['code_index_worker_status'] = {
      configured: { mode: 'exact', workers: 3 },
      environment_override_workers: 7,
      effective_workers: 7,
      available_logical_cpus: 12,
      memory_safe_workers: 10,
      limiting_reason: 'environment_override',
    };
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(overridden)));
    const events = userEvent.setup();
    renderSettings();

    expect(within(await findRow('user.code_index_worker_status.environment_override_workers')).getByText('7')).toBeTruthy();
    await openReview(events, WORKERS);
    expect(
      screen.getByText(
        'TRACEDECAY_INDEX_WORKERS=7 overrides the persisted worker selection for this running daemon.',
      ),
    ).toBeTruthy();
  });

  it('states that an exact selection is judged on restart when admission limits are unavailable', async () => {
    const unavailable = settings();
    const user = settingsBody(unavailable)['user'] as Record<string, unknown>;
    user['code_index_worker_status'] = null;
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(unavailable)));
    const events = userEvent.setup();
    renderSettings();

    await openReview(events, WORKERS);
    expect(
      screen.getByText(
        'Current CPU and memory admission limits are unavailable; an exact count is judged when the daemon restarts.',
      ),
    ).toBeTruthy();
  });

  it('blocks a stale project change before sending the patch', async () => {
    let getCount = 0;
    const methods: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (isSettingsRoute(url)) methods.push(`${method} ${url}`);
        if (url === '/api/settings' && method === 'GET') {
          getCount += 1;
          if (getCount === 1) return jsonResponse(settings());
          const current = updatedSettings('rev-43');
          const project = settingsBody(current)['project'] as Record<string, unknown>;
          const config = project['config'] as Record<string, unknown>;
          config['max_file_size'] = 4096;
          return jsonResponse(current);
        }
        throw new Error(`unexpected request ${method} ${url}`);
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    await openReview(user, MAX_FILE_SIZE);
    const input = proposal(MAX_FILE_SIZE);
    await user.clear(input);
    await user.type(input, '2097152');
    await user.click(screen.getByRole('button', { name: 'Review project change' }));
    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this change against configuration revision rev-42/,
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Apply project settings' }));

    expect(
      await screen.findByText(
        'Another writer saved project settings after this form loaded. Your draft was based on rev-42; the current authority is rev-43. Nothing was applied.',
      ),
    ).toBeTruthy();
    expect(reviewReadout()).toBe('conflict');
    await user.click(screen.getByRole('button', { name: 'Load current values' }));
    expect(await screen.findByDisplayValue('4096')).toBeTruthy();
    expect(within(row(MAX_FILE_SIZE)).getByText('4,096')).toBeTruthy();
    expect(methods).toEqual(['GET /api/settings', 'GET /api/settings', 'GET /api/settings']);
  });

  it('locks both configuration_batch scopes when the effect is withdrawn, and says why', async () => {
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        if (isSettingsRoute(String(input)))
          calls.push(`${init?.method ?? 'GET'} ${String(input)}`);
        return jsonResponse(settingsWithout('configuration_batch'));
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    expect((await findRow(MAX_FILE_SIZE)).dataset['write']).toBe('locked');
    expect(row(WATCHER_DEBOUNCE).dataset['write']).toBe('locked');
    // The worker resource keeps its distinct ProfileSessions authority.
    expect(row(WORKERS).dataset['write']).toBe('writable');

    const panel = await openReview(user, MAX_FILE_SIZE);
    expect(within(panel).getByText(/this dashboard is not authorized to apply project settings/)).toBeTruthy();
    expect(panel.querySelector('[data-settings-gate="unauthorized"]')).toBeTruthy();
    expect(screen.queryByLabelText(`Proposed value for ${MAX_FILE_SIZE}`)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Review project change' })).toBeNull();
    expect(calls).toEqual(['GET /api/settings']);
  });

  it('reports a withdrawn authority as unavailable rather than a failed write', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url === '/api/settings' && method === 'GET') return jsonResponse(settings());
        if (url === '/api/settings/project' && method === 'PATCH') {
          return jsonResponse(
            {
              code: 'configuration_authority_unavailable',
              detail: 'configuration authority is unavailable',
            },
            503,
          );
        }
        throw new Error(`unexpected request ${method} ${url}`);
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    await openReview(user, MAX_FILE_SIZE);
    const input = proposal(MAX_FILE_SIZE);
    await user.clear(input);
    await user.type(input, '2097152');
    await user.click(screen.getByRole('button', { name: 'Review project change' }));
    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this change against configuration revision rev-42/,
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Apply project settings' }));

    expect(
      await screen.findByText('Nothing was applied: configuration authority is unavailable.'),
    ).toBeTruthy();
    expect(reviewReadout()).toBe('withdrawn');
    expect(screen.getByRole('button', { name: 'Retry project settings' })).toBeTruthy();
    expect(screen.queryByText('Project settings saved')).toBeNull();
  });

  it('shows client validation live and never sends an invalid patch', async () => {
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        if (isSettingsRoute(String(input)))
          calls.push(`${init?.method ?? 'GET'} ${String(input)}`);
        return jsonResponse(settings());
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    await openReview(user, POLL_SECS);
    const poll = proposal(POLL_SECS);
    await user.clear(poll);
    await user.type(poll, '59');

    expect(screen.getAllByText('auto_track_pr_poll_secs must be at least 60 seconds').length).toBeGreaterThan(0);
    expect(document.querySelector('[data-settings-validation]')?.getAttribute('data-settings-validation')).toBe('invalid');
    expect(poll.getAttribute('aria-invalid')).toBe('true');
    const pollError = poll.getAttribute('aria-describedby');
    expect(pollError).not.toBeNull();
    expect(document.getElementById(pollError ?? '')?.textContent).toBe(
      'auto_track_pr_poll_secs must be at least 60 seconds',
    );
    expect(screen.getByRole('button', { name: 'Review project change' }).hasAttribute('disabled')).toBe(true);
    expect(calls).toEqual(['GET /api/settings']);
  });

  it('surfaces structured server validation errors for a user patch', async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        const body = init?.body ? JSON.parse(String(init.body)) : null;
        if (isSettingsRoute(url)) calls.push({ url, method, body });
        if (url === '/api/settings' && method === 'GET') {
          return jsonResponse(settings());
        }
        if (url === '/api/settings/user' && method === 'PATCH') {
          return jsonResponse(
            {
              detail: 'settings validation failed',
              validation_errors: [
                {
                  field: 'watcher_debounce',
                  message: 'watcher debounce is denied by the active profile policy',
                },
              ],
            },
            400,
          );
        }
        throw new Error(`unexpected request ${method} ${url}`);
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    await openReview(user, WATCHER_DEBOUNCE);
    const debounce = proposal(WATCHER_DEBOUNCE);
    await user.clear(debounce);
    await user.type(debounce, '15s');
    await user.click(screen.getByRole('button', { name: 'Review user change' }));
    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this change against configuration revision user-rev-7/,
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Apply user settings' }));

    expect(
      (await screen.findAllByText('watcher debounce is denied by the active profile policy')).length,
    ).toBeGreaterThan(0);
    expect(screen.getByText(/The daemon rejected this user settings change/)).toBeTruthy();
    expect(reviewReadout()).toBe('rejected');
    expect(debounce.getAttribute('aria-invalid')).toBe('true');
    const debounceError = debounce.getAttribute('aria-describedby');
    expect(debounceError).not.toBeNull();
    expect(document.getElementById(debounceError ?? '')?.textContent).toBe(
      'watcher debounce is denied by the active profile policy',
    );
    // The frozen review is gone; the draft is intact for another attempt.
    expect(document.querySelector('[data-settings-stage]')).toBeNull();
    expect(calls).toEqual([
      { method: 'GET', url: '/api/settings', body: null },
      { method: 'GET', url: '/api/settings', body: null },
      {
        method: 'PATCH',
        url: '/api/settings/user',
        body: {
          expected_revision_id: 'user-rev-7',
          idempotency_key: expect.stringMatching(/^idempotency\.dashboard-settings\./),
          watcher_debounce: '15s',
        },
      },
    ]);
  });

  it('pins the review while its write is in flight so the verdict cannot be hidden', async () => {
    let releasePatch: (() => void) | null = null;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url === '/api/settings' && method === 'GET') return jsonResponse(settings());
        if (url === '/api/settings/project' && method === 'PATCH') {
          await new Promise<void>((resolve) => {
            releasePatch = resolve;
          });
          return jsonResponse(
            { code: 'configuration_authority_unavailable', detail: 'configuration authority is unavailable' },
            503,
          );
        }
        throw new Error(`unexpected request ${method} ${url}`);
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    await openReview(user, MAX_FILE_SIZE);
    const input = proposal(MAX_FILE_SIZE);
    await user.clear(input);
    await user.type(input, '2097152');
    await user.click(screen.getByRole('button', { name: 'Review project change' }));
    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this change against configuration revision rev-42/,
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Apply project settings' }));
    await waitFor(() => expect(reviewReadout()).toBe('applying'));

    // Neither the close control, Escape, nor selecting another row may take
    // the panel away while the write is out.
    expect(screen.getByRole('button', { name: 'Close review' }).hasAttribute('disabled')).toBe(true);
    await user.keyboard('{Escape}');
    await user.click(row(POLL_SECS));
    expect(document.querySelector(`[data-settings-review="${MAX_FILE_SIZE}"]`)).toBeTruthy();
    expect(row(POLL_SECS).getAttribute('aria-selected')).toBe('false');

    act(() => releasePatch?.());
    expect(
      await screen.findByText('Nothing was applied: configuration authority is unavailable.'),
    ).toBeTruthy();
    expect(reviewReadout()).toBe('withdrawn');
  });

  it('states a key without a write path as read-only rather than locked or denied', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));
    const user = userEvent.setup();
    renderSettings();

    expect((await findRow('storage.store_root')).dataset['write']).toBe('no_write_path');
    const panel = await openReview(user, 'storage.store_root');
    expect(panel.querySelector('[data-settings-gate="no_write_path"]')).toBeTruthy();
    expect(within(panel).getByText(/no settings PATCH route addresses it/)).toBeTruthy();
    expect(within(panel).queryByRole('button', { name: /Review/ })).toBeNull();
    expect(within(panel).queryByRole('textbox')).toBeNull();
  });

  it('opens a review from the keyboard, closes it with Escape, and returns focus to the row', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));
    const user = userEvent.setup();
    renderSettings();

    const first = await findRow('project.config.context_scout');
    act(() => first.focus());
    await user.keyboard('{ArrowDown}');
    const second = document.activeElement as HTMLElement;
    expect(second.getAttribute('role')).toBe('row');
    expect(second.dataset['key']).not.toBe('project.config.context_scout');
    await user.keyboard('{Enter}');
    const key = second.dataset['key'] ?? '';
    expect(document.querySelector(`[data-settings-review="${key}"]`)).toBeTruthy();
    expect(second.getAttribute('aria-selected')).toBe('true');

    await user.keyboard('{Escape}');
    expect(document.querySelector('[data-settings-review]')).toBeNull();
    expect(document.activeElement).toBe(row(key));
  });

  it('filters rows by key or value and states a no-match rather than an empty table', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));
    const user = userEvent.setup();
    renderSettings();

    await findRow(MAX_FILE_SIZE);
    const filter = screen.getByLabelText('Filter configuration');
    await user.type(filter, 'poll');
    expect([...document.querySelectorAll('[role="row"][data-key]')].map((element) => element.getAttribute('data-key'))).toEqual([POLL_SECS]);
    expect(screen.getByText('1 of 59 settings')).toBeTruthy();

    await user.clear(filter);
    await user.type(filter, 'zzzz-no-such-key');
    expect(screen.getByText('no key or value matches “zzzz-no-such-key”')).toBeTruthy();
    expect(document.querySelector('[role="grid"]')).toBeNull();
  });
});

/**
 * Project and ordinary user writes route through the selected project's
 * gateway. The ProfileSessions worker selection is profile-global and must
 * remain governed by its own advertised operation.
 */
describe('Settings scope authority', () => {
  afterEach(() => useScope.getState().selectAllProjects());

  it('keeps the profile worker key writable in a selected non-active project', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_other',
        label: 'Other project',
        activation: 'selected',
      },
    });
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        if (isSettingsRoute(String(input)))
          calls.push(`${init?.method ?? 'GET'} ${String(input)}`);
        return jsonResponse(settings());
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    expect((await findRow(MAX_FILE_SIZE)).dataset['write']).toBe('locked');
    expect(row(WATCHER_DEBOUNCE).dataset['write']).toBe('locked');
    expect(row(WORKERS).dataset['write']).toBe('writable');
    expect(document.querySelector('[data-settings-scope="read_only"]')).toBeTruthy();

    const panel = await openReview(user, MAX_FILE_SIZE);
    const gate = panel.querySelector('[data-settings-gate="read_only"]');
    expect(gate?.textContent).toContain('is not the active project');
    expect(gate?.textContent).toContain('Switch scope to the active project');
    // The permission accusation is the wrong one here and must be absent.
    expect(gate?.textContent).not.toMatch(/not authorized/i);

    // The read is legitimate in any scope; nothing else went out.
    expect(calls.filter((call) => !call.startsWith('GET '))).toEqual([]);
  });

  it('keeps the permission accusation for a scope the daemon does not advertise', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_active',
        label: 'Active project',
        activation: 'active',
      },
    });
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settingsWithout('configuration_batch'))));
    const user = userEvent.setup();
    renderSettings();

    expect((await findRow(WATCHER_DEBOUNCE)).dataset['write']).toBe('locked');
    expect(row(WORKERS).dataset['write']).toBe('writable');
    const panel = await openReview(user, WATCHER_DEBOUNCE);
    expect(panel.querySelector('[data-settings-gate="unauthorized"]')?.textContent).toContain(
      'not authorized',
    );
  });

  it('does not claim a refusal while the scope activation is unresolved', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_link',
        label: 'Linked project',
        activation: 'unresolved',
      },
    });
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));
    const user = userEvent.setup();
    renderSettings();

    expect((await findRow(MAX_FILE_SIZE)).dataset['write']).toBe('locked');
    const panel = await openReview(user, MAX_FILE_SIZE);
    const gate = panel.querySelector('[data-settings-gate="unknown"]');
    expect(gate?.textContent).toContain('not known yet');
    expect(gate?.textContent).not.toMatch(/not authorized|read-only project/i);
    expect(document.querySelector('[data-settings-scope="unknown"]')).toBeTruthy();
  });

  it('names the write target in the active project and the profile for the worker key', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_active',
        label: 'Active project',
        activation: 'active',
      },
    });
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));
    const user = userEvent.setup();
    renderSettings();

    let panel = await openReview(user, MAX_FILE_SIZE);
    expect(within(panel).getByText('applies to Active project')).toBeTruthy();
    panel = await openReview(user, WORKERS);
    expect(within(panel).getByText('applies to your TraceDecay profile')).toBeTruthy();
  });

  /**
   * Project and ordinary user settings take their target from the reconciled
   * scope, so a deep link cannot choose their write target.
   */
  it('names the registry label once the scope is reconciled', async () => {
    useScope.getState().selectProject('proj_active', 'Scratch sandbox', 'unresolved');
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));
    const user = userEvent.setup();
    renderSettings();

    expect((await findRow(MAX_FILE_SIZE)).dataset['write']).toBe('locked');

    act(() =>
      useScope.getState().reconcileScope({
        state: 'measured',
        label: 'Production',
        isActive: true,
      }),
    );

    await waitFor(() => expect(row(MAX_FILE_SIZE).dataset['write']).toBe('writable'));
    const panel = await openReview(user, MAX_FILE_SIZE);
    expect(within(panel).getByText('applies to Production')).toBeTruthy();
    expect(document.body.textContent).not.toContain('Scratch sandbox');
  });

  it('locks the worker key only when its ProfileSessions action is absent', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(settingsWithout('profile_code_index_worker_selection'))),
    );
    const user = userEvent.setup();
    renderSettings();

    expect((await findRow(WORKERS)).dataset['write']).toBe('locked');
    expect(row(MAX_FILE_SIZE).dataset['write']).toBe('writable');
    expect(row(WATCHER_DEBOUNCE).dataset['write']).toBe('writable');
    const panel = await openReview(user, WORKERS);
    expect(
      within(panel).getByText(/this dashboard is not authorized to apply code-index worker settings/),
    ).toBeTruthy();
  });
});

describe('Settings response authority', () => {
  it('classifies a malformed refresh payload as a settings contract violation', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse([])));

    const result = await applySettingsMutation({
      writability: ACTIVE_SCOPE,
      scope: 'project',
      expectedRevisionId: 'rev-42',
      idempotencyKey: 'settings-idempotency-fixture',
      readUrl: '/api/settings',
      patchUrl: '/api/settings/project',
      patch: { max_file_size: 2_097_152 },
    });

    expect(result).toEqual({
      outcome: 'protocol_error',
      authority: 'GET /api/settings',
      detail:
        'GET /api/settings violated the settings contract: expected an envelope carrying a payload.',
    });
  });

  it('classifies an incomplete refresh payload as a settings contract violation', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ payload: {} })));

    const result = await applySettingsMutation({
      writability: ACTIVE_SCOPE,
      scope: 'project',
      expectedRevisionId: 'rev-42',
      idempotencyKey: 'settings-idempotency-fixture',
      readUrl: '/api/settings',
      patchUrl: '/api/settings/project',
      patch: { max_file_size: 2_097_152 },
    });

    expect(result).toEqual({
      outcome: 'protocol_error',
      authority: 'GET /api/settings',
      detail:
        'GET /api/settings violated the settings contract: the response omitted editable values or revision identity.',
    });
  });

  it('classifies a malformed update payload as a settings contract violation', async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockResolvedValueOnce(jsonResponse(settings()))
        .mockResolvedValueOnce(jsonResponse([])),
    );

    const result = await applySettingsMutation({
      writability: ACTIVE_SCOPE,
      scope: 'project',
      expectedRevisionId: 'rev-42',
      idempotencyKey: 'settings-idempotency-fixture',
      readUrl: '/api/settings',
      patchUrl: '/api/settings/project',
      patch: { max_file_size: 2_097_152 },
    });

    expect(result).toEqual({
      outcome: 'protocol_error',
      authority: 'PATCH /api/settings/project',
      detail:
        'PATCH /api/settings/project violated the settings contract: the response omitted the canonical application effect receipt or current settings.',
    });
  });

  it('names the update authority when required editable fields are omitted', async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockResolvedValueOnce(jsonResponse(settings()))
        .mockResolvedValueOnce(jsonResponse({ payload: {} })),
    );

    const result = await applySettingsMutation({
      writability: ACTIVE_SCOPE,
      scope: 'user',
      expectedRevisionId: 'user-rev-7',
      idempotencyKey: 'settings-idempotency-fixture',
      readUrl: '/api/settings',
      patchUrl: '/api/settings/user',
      patch: { watcher_debounce: '15s' },
    });

    expect(result).toEqual({
      outcome: 'protocol_error',
      authority: 'PATCH /api/settings/user',
      detail:
        'PATCH /api/settings/user violated the settings contract: the response omitted editable values or revision identity.',
    });
  });

  it('names the read authority when the daemon returns non-JSON', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('not json')));

    const result = await applySettingsMutation({
      writability: ACTIVE_SCOPE,
      scope: 'project',
      expectedRevisionId: 'rev-42',
      idempotencyKey: 'settings-idempotency-fixture',
      readUrl: '/api/settings',
      patchUrl: '/api/settings/project',
      patch: { max_file_size: 2_097_152 },
    });

    expect(result).toEqual({
      outcome: 'protocol_error',
      authority: 'GET /api/settings',
      detail: 'GET /api/settings violated the settings contract: expected JSON.',
    });
  });

  it('withdraws editing when the read names no revision, and says so on the row', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        // A revision the contract admits as a string but that names no
        // revision, so the editor has nothing to compare and set against.
        const envelope = settings();
        const project = settingsBody(envelope)['project'] as Record<string, unknown>;
        project['configuration_revision_id'] = '';
        return jsonResponse(envelope);
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    expect(reviewReadout()).toBeUndefined();
    await findRow(MAX_FILE_SIZE);
    expect(reviewReadout()).toBe('unavailable');
    // Every bound key is locked, by the read rather than by a scope gate; an
    // unbound key still has no write path.
    expect(row(MAX_FILE_SIZE).dataset['write']).toBe('locked');
    expect(row(WORKERS).dataset['write']).toBe('locked');
    expect(row('storage.store_root').dataset['write']).toBe('no_write_path');
    const panel = await openReview(user, MAX_FILE_SIZE);
    expect(panel.querySelector('[data-settings-gate="editor_unavailable"]')?.textContent).toContain(
      'GET /api/settings named no configuration revision to hold a write against',
    );
    expect(screen.queryByLabelText(`Proposed value for ${MAX_FILE_SIZE}`)).toBeNull();
    // The inspector prints the empty revision as a stated absence.
    expect(screen.getAllByText('not stated').length).toBeGreaterThan(0);
  });

  it('refuses a response that omits a field the settings contract requires', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        const envelope = settings();
        const project = settingsBody(envelope)['project'] as Record<string, unknown>;
        delete project['configuration_revision_id'];
        return jsonResponse(envelope);
      }),
    );

    renderSettings();

    // A payload that does not satisfy the generated contract is an unsupported
    // schema, not settings to render with one group quietly missing.
    expect(await screen.findByText('Unsupported schema')).toBeTruthy();
  });

  it('renders automation source failure without presenting a global fallback as effective', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        const envelope = settings();
        settingsBody(envelope)['automation'] = {
          config_endpoint: '/api/plugins/holographic/curation/config',
          availability: {
            available: false,
            reason: 'project automation configuration could not be read',
            required_authority: 'project automation configuration',
          },
          source_coverage: {
            global: 'available',
            project: 'error',
            effective: 'unavailable',
          },
        };
        return jsonResponse(envelope);
      }),
    );

    renderSettings();

    expect(await screen.findByText('Automation configuration unavailable')).toBeTruthy();
    expect(screen.getByText('project automation configuration could not be read')).toBeTruthy();
    expect(screen.queryByText('Effective automation config, merged daemon-side')).toBeNull();
  });
});

/**
 * The section rail's jump, against ids this dashboard does not choose.
 *
 * `buildSettingsModel` takes a section's id straight from the payload's
 * top-level key — including keys no `GROUP_META` entry names, which is
 * deliberate, so a group the daemon starts reporting appears rather than
 * vanishes. Those keys reached a `[data-section="${id}"]` selector, so one
 * double quote closed the attribute early and `querySelector` threw
 * `SyntaxError` from inside a click handler: the index stopped navigating, with
 * nothing on screen to say why.
 */
describe('Settings section navigation', () => {
  const AWKWARD = ['odd"group', 'back\\slash', 'has space', "single'quote", '#hash.dot'];

  function sectionsFixture(ids: readonly string[]): HTMLDivElement {
    const container = document.createElement('div');
    for (const id of ids) {
      const section = document.createElement('section');
      section.dataset['section'] = id;
      container.append(section);
    }
    return container;
  }

  it('resolves a section id that no selector could carry unescaped', () => {
    const container = sectionsFixture(AWKWARD);

    for (const id of AWKWARD) {
      const found = findConfigSection(container, id);
      expect(found, `no section resolved for ${id}`).toBeDefined();
      expect(found?.dataset['section']).toBe(id);
    }
  });

  it('resolves nothing for an id no section carries, rather than the first one', () => {
    expect(findConfigSection(sectionsFixture(AWKWARD), 'odd"group')?.dataset['section']).toBe(
      'odd"group',
    );
    expect(findConfigSection(sectionsFixture(AWKWARD), 'absent')).toBeUndefined();
    // Not a prefix or substring match either: `project` must not answer for
    // `project.sync`.
    expect(findConfigSection(sectionsFixture(['project']), 'project.sync')).toBeUndefined();
  });

  it('jumps to the section the rail names and marks the inspected row’s section', async () => {
    const user = userEvent.setup();
    const scrollTo = vi.fn();
    vi.spyOn(Element.prototype, 'scrollTo').mockImplementation(scrollTo);
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));

    renderSettings();
    const navigation = await screen.findByRole('navigation', { name: 'Configuration groups' });
    await findRow(MAX_FILE_SIZE);

    await user.click(within(navigation).getByRole('button', { name: /Storage/ }));
    // Called rather than skipped: `jumpTo` returns without scrolling when the
    // lookup finds nothing, so this separates "resolved the section" from
    // "silently found nothing".
    expect(scrollTo).toHaveBeenCalledTimes(1);

    fireEvent.pointerEnter(row('storage.store_root'));
    expect(within(navigation).getByRole('button', { name: /Storage/ }).getAttribute('aria-current')).toBe('true');
    expect(within(navigation).getByRole('button', { name: /Project/ }).getAttribute('aria-current')).toBeNull();
  });
});

describe('Settings responsive controls', () => {
  it('keeps configuration group navigation available below desktop widths', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));

    renderSettings();

    const navigation = await screen.findByRole('navigation', {
      name: 'Configuration groups',
    });
    expect(navigation.className.split(/\s+/)).not.toContain('hidden');
    expect(within(navigation).getByRole('button', { name: /Project/ })).toBeTruthy();
    expect(within(navigation).getByRole('button', { name: /User/ })).toBeTruthy();
    expect(within(navigation).getByRole('button', { name: /Environment/ })).toBeTruthy();
  });
});

function renderSettings() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  });
  return render(
    <QueryClientProvider client={client}>
      <SettingsPage />
    </QueryClientProvider>,
  );
}

/** The envelope `/api/settings` answers with, as the route serves it. */
function settings(): Record<string, unknown> {
  return structuredClone(FIXTURES['/api/settings']) as Record<string, unknown>;
}

/** The settings groups inside an envelope this test is about to edit. */
function settingsBody(envelope: Record<string, unknown>): Record<string, unknown> {
  return envelope['payload'] as Record<string, unknown>;
}

function updatedSettings(revision: string): Record<string, unknown> {
  const value = settings();
  const project = settingsBody(value)['project'] as Record<string, unknown>;
  const config = project['config'] as Record<string, unknown>;
  project['configuration_revision_id'] = revision;
  config['max_file_size'] = 2_097_152;
  return value;
}

function updatedWorkerSettings(revision: string): Record<string, unknown> {
  const value = settings();
  const user = settingsBody(value)['user'] as Record<string, unknown>;
  user['code_index_worker_configuration_revision_id'] = revision;
  user['code_index_workers'] = { mode: 'exact', workers: 4 };
  settingsBody(value)['restart_recommended'] = true;
  return value;
}

/** The same envelope with one write scope withdrawn, as a dashboard without
 * that scope's authority receives it. */
function settingsWithout(operation: string): Record<string, unknown> {
  const value = settings();
  value['legal_actions'] = (
    value['legal_actions'] as Array<{ kind: string; operation: string }>
  ).filter((action) => action.operation !== operation);
  return value;
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}
