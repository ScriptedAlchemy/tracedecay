import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import { useScope, type ScopeWritability } from '../../data/scope/store.ts';
import { SettingsPage, findConfigSection } from './SettingsPage.tsx';
import { applySettingsMutation } from './settingsMutation.ts';

/** The dashboard pointed at the project the daemon has active — the scope every
 * case below is about something other than. */
const ACTIVE_SCOPE: ScopeWritability = { state: 'writable', target: 'tracedecay' };

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
 * reads `/api/capabilities` now, for the multi-root panel, which is neither
 * part of that protocol nor able to affect it — so the recorders below keep
 * only settings traffic. The assertion still fails on an extra settings read,
 * a duplicate patch, or a write the page should not have sent; it just stops
 * doubling as a ledger of every route the page touches.
 */
function isSettingsRoute(url: string): boolean {
  return new URL(url, 'http://localhost').pathname.startsWith('/api/settings');
}

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
      idempotency_key: expect.any(String),
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

  it('withdraws both editable scopes when the configuration effect is gone', async () => {
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        if (isSettingsRoute(String(input)))
          calls.push(`${init?.method ?? 'GET'} ${String(input)}`);
        return jsonResponse(settingsWithout('configuration_batch'));
      }),
    );
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

    // Both editable scopes settle through the one cataloged daemon
    // configuration effect (a user write dispatches the same
    // configuration_batch), so withdrawing it takes the user editor
    // read-only as well — offering the form would promise a 503.
    expect(
      await screen.findByText(
        'Read-only · this dashboard is not authorized to apply user settings',
      ),
    ).toBeTruthy();
    expect(screen.getByLabelText('Watcher debounce').closest('fieldset')?.disabled).toBe(
      true,
    );
    expect(screen.queryByRole('button', { name: 'Review user changes' })).toBeNull();
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
        if (isSettingsRoute(String(input)))
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
          idempotency_key: expect.stringMatching(/^idempotency\.dashboard-settings\./),
          watcher_debounce: '15s',
        },
      },
    ]);
  });
});

/**
 * Both patch routes are addressed through the project gateway, so the scope the
 * dashboard is pointed at decides whether either editor can be written at all —
 * independently of whether the daemon advertises the apply action.
 *
 * Conflating the two is what this covers. The read-only banner used to say "this
 * dashboard is not authorized to apply project settings" for every reason it
 * could be read-only, which under a selected project sent the reader looking for
 * a permission problem that did not exist while the actual remedy — switch scope
 * — went unmentioned.
 */
describe('Settings scope authority', () => {
  const fieldset = (label: string) =>
    screen.getByLabelText(label).closest('fieldset') as HTMLFieldSetElement;

  afterEach(() => useScope.getState().selectAllProjects());

  it('takes both editors read-only in a selected non-active project, naming the scope', async () => {
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
    renderSettings();

    // Both scopes, because the obstacle is the gateway rather than either
    // scope's own authority — the envelope advertises both apply actions here.
    const banners = await waitFor(() => {
      const found = document.querySelectorAll('[data-settings-gate="read_only"]');
      expect(found).toHaveLength(2);
      return [...found];
    });
    for (const banner of banners) {
      expect(banner.textContent).toContain('is not the active project');
      expect(banner.textContent).toContain('Switch scope to the active project');
      // The permission accusation is the wrong one here and must be absent.
      expect(banner.textContent).not.toMatch(/not authorized/i);
    }
    expect(fieldset('Maximum file size (bytes)').disabled).toBe(true);
    expect(fieldset('Watcher debounce').disabled).toBe(true);
    expect(screen.queryByRole('button', { name: 'Review project changes' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Review user changes' })).toBeNull();

    // The read is legitimate in any scope; nothing else went out.
    expect(calls.filter((call) => !call.startsWith('GET '))).toEqual([]);
  });

  it('keeps the permission accusation for a scope the daemon does not advertise', async () => {
    // The active project, so the scope is not the obstacle and the banner must
    // still name the authority that is.
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_active',
        label: 'Active project',
        activation: 'active',
      },
    });
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settingsWithout('configuration_batch'))));
    renderSettings();

    expect(
      await screen.findByText(
        'Read-only · this dashboard is not authorized to apply project settings',
      ),
    ).toBeTruthy();
    // Both editable scopes settle through the one cataloged configuration
    // effect, so its withdrawal takes the user editor read-only too.
    expect(fieldset('Watcher debounce').disabled).toBe(true);
    expect(
      document.querySelector('[data-settings-gate="unauthorized"]')?.textContent,
    ).toContain('not authorized');
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
    renderSettings();

    const banner = await waitFor(() => {
      const found = document.querySelector('[data-settings-gate="unknown"]');
      expect(found).toBeTruthy();
      return found as Element;
    });
    expect(banner.textContent).toContain('not known yet');
    expect(banner.textContent).not.toMatch(/not authorized|read-only project/i);
  });

  it('names the write target in the active project', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_active',
        label: 'Active project',
        activation: 'active',
      },
    });
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));
    renderSettings();

    await screen.findByRole('button', { name: 'Review project changes' });
    const notes = [...document.querySelectorAll('[data-settings-gate="writable"]')];
    expect(notes).toHaveLength(2);
    for (const note of notes) expect(note.textContent).toBe('Applies to Active project.');
  });

  /**
   * Both editors take their target from the reconciled scope, so a deep link
   * cannot choose the name shown beside the settings it is about to change.
   */
  it('names the registry label in both write targets, not a spoofed deep-link label', async () => {
    useScope.getState().selectProject('proj_active', 'Scratch sandbox', 'unresolved');
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));
    renderSettings();

    // Unresolved first: unknown authority, and no claim either way.
    const unknown = await waitFor(() => {
      const found = [...document.querySelectorAll('[data-settings-gate="unknown"]')];
      expect(found).toHaveLength(2);
      return found;
    });
    for (const note of unknown) expect(note.textContent).toContain('not known yet');

    act(() =>
      useScope.getState().reconcileScope({
        state: 'measured',
        label: 'Production',
        isActive: true,
      }),
    );

    await screen.findByRole('button', { name: 'Review project changes' });
    const notes = [...document.querySelectorAll('[data-settings-gate="writable"]')];
    expect(notes).toHaveLength(2);
    for (const note of notes) expect(note.textContent).toBe('Applies to Production.');
    expect(document.body.textContent).not.toContain('Scratch sandbox');
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
        'Settings editing requires project configuration values and configuration_revision_id from GET /api/settings, plus user settings and configuration_revision_id from the same authority. The response omitted at least one required field.',
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

/**
 * The section index's jump, against ids this dashboard does not choose.
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
  /**
   * The lookup on its own, against ids the payload cannot currently carry.
   *
   * Asserted here rather than through the page because it cannot be reached
   * through the page: the section ids come from the parsed payload's top-level
   * keys, and `SettingsPayloadV1Schema` is a plain `z.object`, so zod strips
   * every key the contract does not name. A test that injected `odd"group` into
   * the fixture would exercise nothing — the key never survives the parse — and
   * would read as coverage of a live defect that is not live. What is real is the
   * hazard: `buildSettingsModel` accepts `unknown` and takes ids from whatever
   * keys it finds, so the selector's safety rests on a parse step outside it.
   */
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
    expect(findConfigSection(sectionsFixture(AWKWARD), 'absent')).toBeUndefined();
    // Not a prefix or substring match either: `project` must not answer for
    // `project.sync`.
    expect(findConfigSection(sectionsFixture(['project']), 'project.sync')).toBeUndefined();
  });

  /**
   * The jump end to end, which no test could reach before: jsdom implements no
   * element scrolling, so `container.scrollTo` threw and the section index's one
   * behavior went uncovered.
   */
  it('jumps to the section the index names', async () => {
    const user = userEvent.setup();
    const scrollTo = vi.fn();
    vi.spyOn(Element.prototype, 'scrollTo').mockImplementation(scrollTo);
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(settings())));

    renderSettings();
    const navigation = await screen.findByRole('navigation', { name: 'Configuration groups' });

    await user.click(within(navigation).getByRole('button', { name: /Project/ }));

    // Called rather than skipped: `jumpTo` returns without scrolling when the
    // lookup finds nothing, so this separates "resolved the section" from
    // "silently found nothing".
    expect(scrollTo).toHaveBeenCalledTimes(1);
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

  /**
   * The premise that keeps section ids safe to put in a selector.
   *
   * `jumpTo` builds `[data-section=…]` out of a section id, which is only safe
   * because the ids are the keys of `SettingsPayloadV1Schema` — a closed
   * `z.object` over Rust field names, which strips everything else before the
   * surface sees it. So the reviewed concern (a daemon-chosen key containing a
   * quote or a backslash) is unreachable today, and this pins the reason
   * rather than staging a payload production cannot produce: if the generated
   * schema ever stops stripping, or grows a map-valued section, this fails.
   *
   * Pinned here rather than defended with `CSS.escape` at the call site,
   * because escaping would buy nothing against a closed contract and would
   * introduce a real dependency on a global jsdom does not define.
   */
  it('admits no section id that a selector could not hold', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        const envelope = settings();
        const body = settingsBody(envelope);
        body['weird"key'] = { enabled: true };
        body['back\\slash'] = { enabled: true };
        return jsonResponse(envelope);
      }),
    );
    renderSettings();
    await screen.findByRole('navigation', { name: 'Configuration groups' });

    const ids = [...document.querySelectorAll('[data-section]')].map(
      (node) => node.getAttribute('data-section') ?? '',
    );
    expect(ids.length).toBeGreaterThan(0);
    for (const id of ids) {
      // A CSS identifier, so the selector `jumpTo` builds around it parses.
      expect(id).toMatch(/^[a-z_][a-z0-9_]*$/i);
      expect(() => document.querySelector(`[data-section="${id}"]`)).not.toThrow();
    }
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

describe('Settings header identity stamps', () => {
  /**
   * The live regression: the project and user groups of a real payload carry
   * the SAME snapshot and revision ids, and the header strip rendered the
   * SNAPSHOT/REVISION pair once per group — a doubled header row and duplicate
   * `label:value` React keys under it. One identity is one stamp, and React
   * must have nothing to warn about while the strip renders.
   */
  it('renders a shared snapshot identity exactly once, with unique keys', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        const envelope = settings();
        const body = settingsBody(envelope);
        const project = body['project'] as Record<string, unknown>;
        const user = body['user'] as Record<string, unknown>;
        user['configuration_snapshot_id'] = project['configuration_snapshot_id'];
        user['configuration_revision_id'] = project['configuration_revision_id'];
        return jsonResponse(envelope);
      }),
    );
    renderSettings();
    await screen.findAllByText('snap-42');

    // One SNAPSHOT cell and one REVISION cell in the header strip, however
    // many groups pin them. Scoped to the header: the body legitimately
    // renders each group's configuration_snapshot_id row as a setting.
    const header = document.querySelector<HTMLElement>('[data-workspace-header]');
    expect(header).not.toBeNull();
    expect(within(header as HTMLElement).getAllByText('snap-42')).toHaveLength(1);
    expect(within(header as HTMLElement).getAllByText('rev-42')).toHaveLength(1);
    expect(within(header as HTMLElement).getAllByText('snapshot')).toHaveLength(1);
    expect(within(header as HTMLElement).getAllByText('revision')).toHaveLength(1);

    const duplicateKeyWarnings = consoleError.mock.calls.filter((call) =>
      String(call[0]).includes('two children with the same key'),
    );
    expect(duplicateKeyWarnings).toEqual([]);
    consoleError.mockRestore();
  });
});
