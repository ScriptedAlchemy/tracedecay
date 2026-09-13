/**
 * The registry key authority, and the invalidation that has to reach it.
 *
 * The defect these guard is silent and has no visible symptom until it matters:
 * four surfaces read the project registry, each had its own query key, and the
 * SSE `project_registry_changed` handler named exactly one of them. The scope
 * bar's key was not the one, and the scope bar has no poll — so a project rename
 * or an active-project switch left it holding the pre-change answer for the rest
 * of the session, and it is the read that decides whether write controls are
 * offered.
 *
 * So the assertions here are about reachability rather than shape: every key a
 * registry reader uses must be one the invalidation actually matches.
 */
import { QueryClient } from '@tanstack/react-query';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { targetedInvalidationKeys } from '../sse/useEvents.tsx';
import type { SseBatch } from '../sse/types.ts';
import {
  PROJECT_NOT_FOUND,
  projectRegistryEntryKey,
  projectRegistryListKey,
  fetchProjectRegistry,
  registryAnnotation,
  registryReading,
  type ProjectRegistryResult,
} from './projectRegistry.ts';
import {
  ProjectContextPayloadV1Schema,
  type DashboardEnvelopeV1,
  type ProjectContextPayloadV1,
} from '../../contracts/generated.ts';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { responseWithBodyPendingUntilAbort } from '../../test/pendingBody.ts';
import { READ_ONLY_SCOPE_STATUS } from '../scope/store.ts';

function registryBatch(): SseBatch {
  return {
    events: [{ payload: { family: 'project_registry_changed' } }],
  } as unknown as SseBatch;
}

describe('project registry query keys', () => {
  it('keeps entries for different projects apart', () => {
    expect(projectRegistryEntryKey('proj_a')).not.toEqual(projectRegistryEntryKey('proj_b'));
  });

  it('does not collide the listing with an entry', () => {
    expect(projectRegistryListKey).not.toEqual(projectRegistryEntryKey('list'));
  });

  /**
   * The same claim, made against React Query itself rather than against the
   * model of it above. `matches` is this file's reading of prefix matching, and
   * a wrong reading would let both tests pass while the client invalidated
   * nothing — so one test holds real cache entries and checks they were really
   * marked stale.
   */
  it('marks both the listing and each entry stale in a real client', async () => {
    const client = new QueryClient();
    const listing = [...projectRegistryListKey, 'unscoped'];
    const entryA = [...projectRegistryEntryKey('proj_a'), 'unscoped'];
    const entryB = [...projectRegistryEntryKey('proj_b'), 'unscoped'];
    const unrelated = ['doctor', 'report', 'project:proj_a'];
    for (const key of [listing, entryA, entryB, unrelated]) {
      client.setQueryData(key, { outcome: 'ok', data: {} });
    }
    expect(client.getQueryState(listing)?.isInvalidated).toBe(false);

    for (const key of targetedInvalidationKeys(registryBatch())) {
      await client.invalidateQueries({ queryKey: key });
    }

    expect(client.getQueryState(listing)?.isInvalidated).toBe(true);
    expect(client.getQueryState(entryA)?.isInvalidated).toBe(true);
    expect(client.getQueryState(entryB)?.isInvalidated).toBe(true);
    // And no wider than the registry: a rename must not discard the app.
    expect(client.getQueryState(unrelated)?.isInvalidated).toBe(false);
  });

});

function fixturePayload(pathname: string): Record<string, unknown> {
  const fixture = resolveFixture(pathname) as Record<string, unknown>;
  return (fixture.payload ?? fixture) as Record<string, unknown>;
}

describe('fetchProjectRegistry', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('keeps a decoded 503 registry answer in the canonical envelope result', async () => {
    const payload = {
      ...fixturePayload('/api/projects/proj_b'),
      status: 'registry_unavailable',
      error: 'unable to open the global registry',
      is_active: null,
      project: null,
      aliases: [],
      stores: [],
    };
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify(fixtureEnvelope(payload)), { status: 503 })),
    );

    await expect(
      fetchProjectRegistry('/api/projects/proj_b', ProjectContextPayloadV1Schema),
    ).resolves.toMatchObject({
      outcome: 'envelope',
      envelope: { payload: { status: 'registry_unavailable' } },
    });
  });

  it.each([
    ['200', 200],
    ['404', 404],
    ['500', 500],
  ])('maps a typed %s not_found envelope to an absent project', async (_label, status) => {
    const payload = context({
      status: PROJECT_NOT_FOUND,
      error: 'no project registered with id proj_ghost',
    });
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify(fixtureEnvelope(payload)), { status })),
    );

    const result = await fetchProjectRegistry('/api/projects/proj_ghost', ProjectContextPayloadV1Schema);

    expect(result.outcome).toBe('envelope');
    expect(registryReading(result)).toEqual({
      state: 'absent',
      reason: 'no project registered with id proj_ghost',
    });
  });

  it.each([
    ['200', 200],
    ['503', 503],
  ])('keeps a typed %s unavailable registry envelope distinct from an absent project', async (_label, status) => {
    const payload = context({
      status: 'registry_unavailable',
      error: 'registry database could not be opened',
    });
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify(fixtureEnvelope(payload)), { status })),
    );

    const result = await fetchProjectRegistry('/api/projects/proj_ghost', ProjectContextPayloadV1Schema);

    expect(result.outcome).toBe('envelope');
    expect(registryReading(result)).toEqual({ state: 'unknown' });
    expect(registryAnnotation(result)).toBe(
      'registry unavailable · registry database could not be opened',
    );
  });

  it('keeps the typed write refusal ahead of generic envelope handling', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        new Response(
          JSON.stringify({
            status: READ_ONLY_SCOPE_STATUS,
            detail: 'project is read-only outside the active scope',
            project_id: 'proj_ghost',
          }),
          { status: 405 },
        ),
      ),
    );

    await expect(
      fetchProjectRegistry('/api/projects/proj_ghost', ProjectContextPayloadV1Schema, {
        method: 'POST',
      }),
    ).resolves.toEqual({
      outcome: 'transport',
      state: 'locked',
      detail: 'project is read-only outside the active scope',
    });
  });

  it('rejects a bare registry payload instead of treating it as an envelope', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify(fixturePayload('/api/projects/proj_b')), { status: 200 })),
    );

    await expect(
      fetchProjectRegistry('/api/projects/proj_b', ProjectContextPayloadV1Schema),
    ).resolves.toEqual({ outcome: 'transport', state: 'unsupported_schema' });
  });

  it.each([200, 405])(
    'preserves an abort that lands while a %i body is still being read',
    async (status) => {
      vi.stubGlobal(
        'fetch',
        vi.fn(async (_url: string, init?: RequestInit) =>
          responseWithBodyPendingUntilAbort(status, init?.signal),
        ),
      );
      const controller = new AbortController();
      const pending = fetchProjectRegistry('/api/projects', ProjectContextPayloadV1Schema, {
        signal: controller.signal,
      });
      controller.abort();
      await expect(pending).rejects.toThrow(/abort/i);
    },
  );
});

function context(overrides: Partial<ProjectContextPayloadV1>): ProjectContextPayloadV1 {
  return {
    status: 'ok',
    is_active: false,
    project: null,
    aliases: [],
    ...overrides,
  } as ProjectContextPayloadV1;
}

function project(label: string) {
  return {
    canonical_root: '/repo',
    created_at: 1,
    default_branch: 'main',
    display_root: '/repo',
    git_common_dir: null,
    label,
    last_seen_at: 2,
    project_id: 'proj_a',
    project_root: '/repo',
  };
}

function registryEnvelope(
  payload: ProjectContextPayloadV1,
): ProjectRegistryResult<ProjectContextPayloadV1> {
  return {
    outcome: 'envelope',
    envelope: { payload } as DashboardEnvelopeV1<ProjectContextPayloadV1>,
  };
}

describe('registryReading', () => {
  it('measures a not-active answer as a real reading', () => {
    expect(
      registryReading(
        registryEnvelope(context({ is_active: false, project: project('Canonical') })),
      ),
    ).toEqual({ state: 'measured', label: 'Canonical', isActive: false });
  });

  it('carries a null through rather than reading it as a no', () => {
    expect(
      registryReading(registryEnvelope(context({ is_active: null, project: null }))),
    ).toEqual({ state: 'measured', label: null, isActive: null });
  });

  it('treats a non-ok status as no reading at all', () => {
    expect(
      registryReading(registryEnvelope(context({ status: 'missing_registry' }))),
    ).toEqual({ state: 'unknown' });
  });

  it.each([
    ['offline', { outcome: 'transport' as const, state: 'offline' as const }],
    ['unauthorized', { outcome: 'transport' as const, state: 'unauthorized' as const }],
    ['denied', { outcome: 'transport' as const, state: 'denied' as const }],
    ['error', { outcome: 'transport' as const, state: 'error' as const, detail: 'HTTP 500' }],
    [
      'unsupported_schema',
      { outcome: 'transport' as const, state: 'unsupported_schema' as const },
    ],
  ])('reports %s as unknown', (_name, result) => {
    expect(registryReading(result)).toEqual({ state: 'unknown' });
  });

  it('measures not_found as the registry holding no such project', () => {
    expect(
      registryReading(
        registryEnvelope(
          context({
            status: 'not_found',
            error: 'no project registered with id proj_ghost',
          }),
        ),
      ),
    ).toEqual({ state: 'absent', reason: 'no project registered with id proj_ghost' });
  });

  it.each(['missing_registry', 'registry_unavailable'])(
    'reports %s as unknown, not as an absent project',
    (status) => {
      expect(
        registryReading(
          registryEnvelope(
            context({
              status,
              error: 'registry database could not be opened',
            }),
          ),
        ),
      ).toEqual({ state: 'unknown' });
    },
  );

});

describe('registryAnnotation', () => {
  it('marks a name the answer did not confirm', () => {
    expect(registryAnnotation(registryEnvelope(context({ project: null })))).toBe('unconfirmed');
  });

  it('says the read is still in flight rather than presenting the name as settled', () => {
    expect(registryAnnotation(undefined)).toBe('resolving');
  });

  it('names the transport state, and still says the name is unconfirmed', () => {
    expect(registryAnnotation({ outcome: 'transport', state: 'offline' })).toBe('registry offline');
    expect(registryAnnotation({ outcome: 'transport', state: 'error', detail: 'HTTP 500' })).toContain(
      'unconfirmed',
    );
  });

  it("repeats the registry's own sentence rather than restating the status code", () => {
    expect(
      registryAnnotation(
        registryEnvelope(
          context({
            status: 'not_found',
            error: 'no project registered with id proj_ghost',
          }),
        ),
      ),
    ).toBe('not in registry · no project registered with id proj_ghost');

    expect(
      registryAnnotation(
        registryEnvelope(
          context({
            status: 'missing_registry',
            error: 'no registry at /home/x/.tracedecay/registry.db',
          }),
        ),
      ),
    ).toBe('registry unavailable · no registry at /home/x/.tracedecay/registry.db');
  });

  it('still names the state when the payload sent no sentence', () => {
    expect(
      registryAnnotation(registryEnvelope(context({ status: 'registry_unavailable' }))),
    ).toBe('registry unavailable');
  });
});
