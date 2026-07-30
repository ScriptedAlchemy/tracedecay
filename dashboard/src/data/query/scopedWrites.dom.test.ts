/**
 * Every write the dashboard currently exposes, against every scope.
 *
 * The scope authority is only worth having if no control can route around it,
 * so this asserts the negative directly: `fetch` is stubbed to throw if it is
 * called at all, and each write is invoked under a non-active and an unresolved
 * scope. Asserting only on the returned outcome would pass just as happily
 * against a control that dispatched and then reported the gateway's 405 — a
 * different and worse behaviour, since it asks a project to change and relies
 * on the daemon to refuse.
 *
 * The inventory is exhaustive over the product's mutations rather than a
 * sample, because a write added later without a scope gate is precisely the
 * regression this file exists to catch and a sample cannot catch it. The one
 * mutation deliberately absent is Explorer's query lifecycle, which does not
 * route through the project gateway.
 */
import { renderHook, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createElement, type ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import { applySettingsMutation } from '../../workspaces/settings/settingsMutation.ts';
import {
  READ_ONLY_SCOPE_STATUS,
  scopeWritable,
  useScope,
  type DashboardScope,
} from '../scope/store.ts';
import { useSchedulerControl } from './automation.ts';
import { applyDoctorRemediation, previewDoctorRemediation } from './doctor.ts';

const ACTIVE: DashboardScope = {
  kind: 'project',
  projectId: 'proj_active',
  label: 'Active project',
  activation: 'active',
};
const SELECTED: DashboardScope = {
  kind: 'project',
  projectId: 'proj_other',
  label: 'Other project',
  activation: 'selected',
};
const UNRESOLVED: DashboardScope = {
  kind: 'project',
  projectId: 'proj_link',
  label: 'Linked project',
  activation: 'unresolved',
};

/** The two scopes that must never produce a request. */
const BLOCKED = [
  ['a selected non-active project', SELECTED],
  ['an unresolved deep-link project', UNRESOLVED],
] as const;

/** A wire-legal request, so a rejection here is the scope gate rather than the
 * contract parse the gate sits in front of. */
const PREVIEW_REQUEST = {
  operation: 'use-case.application.storage.retention-collect',
  target: { owner_operation: 'storage_retention_collect' },
} as const;

const APPLY_REQUEST = {
  ...PREVIEW_REQUEST,
  preview_id: null,
  idempotency_key: 'idem-scope-test',
  confirmed: true,
} as const;

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  fetchMock = vi.fn(async () => {
    throw new Error('a blocked scope dispatched a request');
  });
  vi.stubGlobal('fetch', fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
});

function queryWrapper({ children }: { children: ReactNode }) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return createElement(QueryClientProvider, { client }, children);
}

/** The reason the scope authority gives for refusing this scope, so each case
 * asserts the control repeated it rather than inventing its own wording. */
function blockedReason(scope: DashboardScope): string {
  const writability = scopeWritable(scope);
  if (writability.state === 'writable') throw new Error('fixture is not blocked');
  return writability.reason;
}

describe('writes under a scope that does not accept them', () => {
  describe.each(BLOCKED)('%s', (_name, scope) => {
    it('does not dispatch the automation scheduler control', async () => {
      // The control reads its scope from the store, which is also what disables
      // the page's button, so the store is what is set here.
      useScope.setState({ scope });
      const { result } = renderHook(() => useSchedulerControl(), { wrapper: queryWrapper });
      expect(result.current.writability.state).not.toBe('writable');

      result.current.mutate(true);

      await waitFor(() => expect(result.current.data).toBeDefined());
      expect(result.current.data?.outcome).toBe('not_dispatched');
      expect(fetchMock).not.toHaveBeenCalled();
    });

    it('does not dispatch a Doctor remediation preview', async () => {
      const result = await previewDoctorRemediation(scope, PREVIEW_REQUEST);

      expect(result.outcome).toBe('not_dispatched');
      if (result.outcome !== 'not_dispatched') throw new Error('unreachable');
      expect(result.writability.reason).toBe(blockedReason(scope));
      expect(fetchMock).not.toHaveBeenCalled();
    });

    it('does not dispatch a Doctor remediation apply', async () => {
      const result = await applyDoctorRemediation(scope, APPLY_REQUEST);

      expect(result.outcome).toBe('not_dispatched');
      if (result.outcome !== 'not_dispatched') throw new Error('unreachable');
      expect(result.writability.reason).toBe(blockedReason(scope));
      expect(fetchMock).not.toHaveBeenCalled();
    });

    /**
     * Settings refuses before its *refresh*, not merely before its PATCH. The
     * mutation re-reads settings to recheck the held revision, and once the
     * control is disabled that read is work nobody asked for — and it would make
     * a disabled control look like it had begun something.
     */
    it('does not dispatch a settings patch, nor its pre-patch refresh', async () => {
      const result = await applySettingsMutation({
        scope: 'project',
        expectedRevisionId: 'rev-42',
        readUrl: '/api/settings',
        patchUrl: '/api/settings/project',
        patch: { max_file_size: 2_097_152 },
        writability: scopeWritable(scope),
      });

      expect(result.outcome).toBe('not_dispatched');
      if (result.outcome !== 'not_dispatched') throw new Error('unreachable');
      expect(result.detail).toContain(blockedReason(scope));
      expect(fetchMock).not.toHaveBeenCalled();
    });
  });
});

describe('writes under a scope that accepts them', () => {
  it.each([
    ['the active project', ACTIVE],
    ['the all-projects aggregate', { kind: 'all' } as DashboardScope],
  ])('dispatches a Doctor remediation under %s', async (_name, scope) => {
    const dispatched = vi.fn(async () => new Response('{}', { status: 503 }));
    vi.stubGlobal('fetch', dispatched);

    const result = await previewDoctorRemediation(scope, PREVIEW_REQUEST);

    expect(result.outcome).not.toBe('not_dispatched');
    expect(dispatched).toHaveBeenCalledTimes(1);
  });

  /** The aggregate is writable and a write under it lands on one project, so
   * the target it reports has to say that rather than name a project. */
  it('states that an aggregate write targets only the active project', () => {
    expect(scopeWritable({ kind: 'all' })).toEqual({
      state: 'writable',
      target: 'the active project',
    });
  });
});

/**
 * Activation is measured from a registry read, so it can be stale by the time a
 * write goes out — the daemon may have activated a different project in
 * between. The refusal a dispatched write can still meet therefore has to stay
 * readable, and it is recognized by its body rather than by the status alone.
 */
describe('the gateway refusal a dispatched write can still meet', () => {
  function settingsGateway(patchResponse: Response) {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_url: string, init?: RequestInit) =>
        init?.method === 'PATCH'
          ? patchResponse.clone()
          : new Response(JSON.stringify(FIXTURES['/api/settings']), {
              status: 200,
              headers: { 'content-type': 'application/json' },
            }),
      ),
    );
  }

  const stale = {
    scope: 'project',
    expectedRevisionId: 'rev-42',
    readUrl: '/api/settings',
    patchUrl: '/api/settings/project',
    patch: { max_file_size: 2_097_152 },
    writability: { state: 'writable', target: 'Other project' },
  } as const;

  it('reports a settings patch refused by the gateway as a scope refusal', async () => {
    settingsGateway(
      new Response(
        JSON.stringify({
          status: READ_ONLY_SCOPE_STATUS,
          project_id: 'proj_other',
          detail: 'project proj_other is served read-only',
        }),
        { status: 405, headers: { 'content-type': 'application/json' } },
      ),
    );

    const result = await applySettingsMutation(stale);

    expect(result.outcome).toBe('read_only_scope');
    if (result.outcome !== 'read_only_scope') throw new Error('unreachable');
    expect(result.detail).toBe('Nothing was applied: project proj_other is served read-only.');
  });

  it('reports a 405 the dashboard cannot account for as a plain error', async () => {
    settingsGateway(
      new Response(JSON.stringify({ status: 'method_not_allowed' }), {
        status: 405,
        headers: { 'content-type': 'application/json' },
      }),
    );

    const result = await applySettingsMutation(stale);

    expect(result.outcome).toBe('error');
    if (result.outcome !== 'error') throw new Error('unreachable');
    expect(result.detail).toBe('Settings update failed (HTTP 405).');
  });
});
