import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import { SettingsPage } from './SettingsPage.tsx';
import { applySettingsMutation } from './settingsMutation.ts';

describe('SettingsPage authorized changes', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
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
        calls.push({ url, method, body });
        if (url === '/api/settings' && method === 'GET') {
          return jsonResponse(applied ? updatedSettings('rev-43') : settings());
        }
        if (url === '/api/settings/project' && method === 'PATCH') {
          applied = true;
          const envelope = updatedSettings('rev-43');
          settingsBody(envelope)['resync_recommended'] = true;
          return jsonResponse(envelope);
        }
        throw new Error(`unexpected request ${method} ${url}`);
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    const maxFileSize = await screen.findByLabelText('Maximum file size (bytes)');
    await user.clear(maxFileSize);
    await user.type(maxFileSize, '2097152');
    expect(screen.getByText('Unsaved project changes')).toBeTruthy();
    await user.click(screen.getByRole('button', { name: 'Review project changes' }));

    const dialog = screen.getByRole('dialog', { name: 'Review project settings change' });
    expect(dialog).toBeTruthy();
    expect(within(dialog).getByText(/max_file_size/)).toBeTruthy();
    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this change against configuration revision rev-42/,
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Apply project settings' }));

    expect(await screen.findByText('Project settings saved')).toBeTruthy();
    expect(screen.getByText('Current project values')).toBeTruthy();
    expect(screen.getByText('Resync recommended')).toBeTruthy();
    expect(
      calls.map(({ method, url }) => `${method} ${url}`),
    ).toEqual([
      'GET /api/settings',
      'GET /api/settings',
      'PATCH /api/settings/project',
      'GET /api/settings',
    ]);
    expect(calls[2]?.body).toEqual({
      expected_revision_id: 'rev-42',
      max_file_size: 2_097_152,
    });
  });

  it('blocks a stale project change before sending the patch', async () => {
    let getCount = 0;
    const methods: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        methods.push(`${method} ${url}`);
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

    const maxFileSize = await screen.findByLabelText('Maximum file size (bytes)');
    await user.clear(maxFileSize);
    await user.type(maxFileSize, '2097152');
    await user.click(screen.getByRole('button', { name: 'Review project changes' }));
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
    await user.click(screen.getByRole('button', { name: 'Load current values' }));
    expect(await screen.findByDisplayValue('4096')).toBeTruthy();
    expect(methods).toEqual(['GET /api/settings', 'GET /api/settings', 'GET /api/settings']);
  });

  it('withdraws only the scope the envelope stops authorizing', async () => {
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        calls.push(`${init?.method ?? 'GET'} ${String(input)}`);
        return jsonResponse(settingsWithout('configuration_batch'));
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    expect(
      await screen.findByText(
        'Read-only · this dashboard is not authorized to apply project settings',
      ),
    ).toBeTruthy();
    expect(
      screen.getByLabelText('Maximum file size (bytes)').closest('fieldset')?.disabled,
    ).toBe(true);
    expect(screen.queryByRole('button', { name: 'Review project changes' })).toBeNull();

    // The user scope keeps its own authority, so withdrawing the project
    // action must not take the whole editor read-only with it.
    expect(
      screen.queryByText(
        'Read-only · this dashboard is not authorized to apply user settings',
      ),
    ).toBeNull();
    expect(screen.getByLabelText('Watcher debounce').closest('fieldset')?.disabled).toBe(
      false,
    );
    await user.click(screen.getByRole('button', { name: 'Review user changes' }));
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

    const maxFileSize = await screen.findByLabelText('Maximum file size (bytes)');
    await user.clear(maxFileSize);
    await user.type(maxFileSize, '2097152');
    await user.click(screen.getByRole('button', { name: 'Review project changes' }));
    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this change against configuration revision rev-42/,
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Apply project settings' }));

    expect(
      await screen.findByText(
        'Nothing was applied: configuration authority is unavailable.',
      ),
    ).toBeTruthy();
    expect(screen.queryByText('Project settings saved')).toBeNull();
  });

  it('shows client validation without sending an invalid patch', async () => {
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        calls.push(`${init?.method ?? 'GET'} ${String(input)}`);
        return jsonResponse(settings());
      }),
    );
    const user = userEvent.setup();
    renderSettings();

    const poll = await screen.findByLabelText('PR branch poll interval (seconds)');
    await user.clear(poll);
    await user.type(poll, '59');
    await user.click(screen.getByRole('button', { name: 'Review project changes' }));

    expect(
      screen.getByText('auto_track_pr_poll_secs must be at least 60 seconds'),
    ).toBeTruthy();
    expect(poll.getAttribute('aria-invalid')).toBe('true');
    const pollError = poll.getAttribute('aria-describedby');
    expect(pollError).not.toBeNull();
    expect(document.getElementById(pollError ?? '')?.textContent).toBe(
      'auto_track_pr_poll_secs must be at least 60 seconds',
    );
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
        calls.push({ url, method, body });
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

    const debounce = await screen.findByLabelText('Watcher debounce');
    await user.clear(debounce);
    await user.type(debounce, '15s');
    await user.click(screen.getByRole('button', { name: 'Review user changes' }));
    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this change against configuration revision user-rev-7/,
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Apply user settings' }));

    expect(
      await screen.findByText('watcher debounce is denied by the active profile policy'),
    ).toBeTruthy();
    expect(debounce.getAttribute('aria-invalid')).toBe('true');
    const debounceError = debounce.getAttribute('aria-describedby');
    expect(debounceError).not.toBeNull();
    expect(document.getElementById(debounceError ?? '')?.textContent).toBe(
      'watcher debounce is denied by the active profile policy',
    );
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(calls).toEqual([
      { method: 'GET', url: '/api/settings', body: null },
      { method: 'GET', url: '/api/settings', body: null },
      {
        method: 'PATCH',
        url: '/api/settings/user',
        body: {
          expected_revision_id: 'user-rev-7',
          watcher_debounce: '15s',
        },
      },
    ]);
  });
});

describe('Settings response authority', () => {
  it('classifies a malformed refresh payload as a settings contract violation', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse([])));

    const result = await applySettingsMutation({
      scope: 'project',
      expectedRevisionId: 'rev-42',
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
      scope: 'project',
      expectedRevisionId: 'rev-42',
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
      scope: 'project',
      expectedRevisionId: 'rev-42',
      readUrl: '/api/settings',
      patchUrl: '/api/settings/project',
      patch: { max_file_size: 2_097_152 },
    });

    expect(result).toEqual({
      outcome: 'protocol_error',
      authority: 'PATCH /api/settings/project',
      detail:
        'PATCH /api/settings/project violated the settings contract: expected an envelope carrying a payload.',
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
      scope: 'user',
      expectedRevisionId: 'user-rev-7',
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
      scope: 'project',
      expectedRevisionId: 'rev-42',
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

  it('identifies both authorities when the editable read contract is incomplete', async () => {
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

    renderSettings();

    expect(
      await screen.findByText(
        'Settings editing requires project configuration values and configuration_revision_id from GET /api/settings, plus user settings and user_settings_revision_id from the same authority. The response omitted at least one required field.',
      ),
    ).toBeTruthy();
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
