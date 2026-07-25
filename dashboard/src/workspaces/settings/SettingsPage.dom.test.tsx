import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import { SettingsPage } from './SettingsPage.tsx';

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
          return jsonResponse({
            ...updatedSettings('rev-43'),
            resync_recommended: true,
          });
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
          return jsonResponse(getCount === 1 ? settings() : updatedSettings('rev-43'));
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
        'Configuration changed since this form loaded (held rev-42, current rev-43).',
      ),
    ).toBeTruthy();
    expect(methods).toEqual(['GET /api/settings', 'GET /api/settings']);
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

function settings(): Record<string, unknown> {
  return structuredClone(FIXTURES['/api/settings']) as Record<string, unknown>;
}

function updatedSettings(revision: string): Record<string, unknown> {
  const value = settings();
  const project = value['project'] as Record<string, unknown>;
  const config = project['config'] as Record<string, unknown>;
  project['configuration_revision_id'] = revision;
  config['max_file_size'] = 2_097_152;
  return value;
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}
