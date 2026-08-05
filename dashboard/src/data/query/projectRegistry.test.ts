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
  PROJECT_REGISTRY_ROOT,
  PROJECT_NOT_FOUND,
  projectRegistryEntryKey,
  projectRegistryInvalidationKey,
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
import { READ_ONLY_SCOPE_STATUS } from '../scope/store.ts';

/**
 * React Query's default key matching: a query is invalidated when the
 * invalidation key is a prefix of it.
 *
 * Reimplemented rather than asserted through a QueryClient because the property
 * under test is about the keys themselves, and a test that spun up a client
 * could pass while the keys were unrelated — the client would just invalidate
 * nothing and report success.
 */
function matches(invalidation: readonly unknown[], queryKey: readonly unknown[]): boolean {
  return invalidation.every((segment, index) => queryKey[index] === segment);
}

function registryBatch(): SseBatch {
  return {
    events: [{ payload: { family: 'project_registry_changed' } }],
  } as unknown as SseBatch;
}

describe('project registry query keys', () => {
  it('roots every registry key at the shared prefix', () => {
    expect(projectRegistryListKey[0]).toBe(PROJECT_REGISTRY_ROOT);
    expect(projectRegistryEntryKey('proj_a')[0]).toBe(PROJECT_REGISTRY_ROOT);
    expect(projectRegistryInvalidationKey).toEqual([PROJECT_REGISTRY_ROOT]);
  });

  it('keeps entries for different projects apart', () => {
    expect(projectRegistryEntryKey('proj_a')).not.toEqual(projectRegistryEntryKey('proj_b'));
  });

  it('does not collide the listing with an entry', () => {
    expect(projectRegistryListKey).not.toEqual(projectRegistryEntryKey('list'));
  });

  /**
   * The registry's direct daemon route carries no selected project. Its query
   * key therefore ends in the unscoped token, while the invalidation still has
   * to match the listing and every per-project entry it cannot enumerate.
   */
  it('is reached by the registry invalidation, listing and entries alike', () => {
    const held = [
      [...projectRegistryListKey, 'unscoped'],
      [...projectRegistryEntryKey('proj_a'), 'unscoped'],
      [...projectRegistryEntryKey('proj_b'), 'unscoped'],
    ];
    for (const queryKey of held) {
      expect(matches(projectRegistryInvalidationKey, queryKey)).toBe(true);
    }
  });

  it('is the key a project_registry_changed event actually invalidates', () => {
    // The end-to-end link: the event handler's own output, matched against the
    // keys the readers hold. A literal in the handler used to name only one.
    const keys = targetedInvalidationKeys(registryBatch());
    expect(keys).toContainEqual([...projectRegistryInvalidationKey]);

    const invalidation = keys.find((key) =>
      matches(key, [...projectRegistryListKey, 'unscoped']),
    );
    expect(invalidation).toBeDefined();
    expect(
      matches(invalidation as readonly string[], [
        ...projectRegistryEntryKey('proj_a'),
        'unscoped',
      ]),
    ).toBe(true);
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

  it('does not invalidate the whole cache for a registry change', () => {
    // Prefix matching makes an empty key match everything. The registry root has
    // to be narrower than that, or a rename would discard every read in the app.
    for (const key of targetedInvalidationKeys(registryBatch())) {
      expect(key.length).toBeGreaterThan(0);
    }
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

  it('accepts only the envelope at the daemon-wide project route', async () => {
    const fetchMock = vi.fn(async () =>
      new Response(JSON.stringify(fixtureEnvelope(fixturePayload('/api/projects/proj_b'))), {
        status: 200,
      }),
    );
    vi.stubGlobal('fetch', fetchMock);

    const result = await fetchProjectRegistry(
      '/api/projects/proj_b',
      ProjectContextPayloadV1Schema,
    );

    expect(result.outcome).toBe('envelope');
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/projects/proj_b',
      expect.objectContaining({ headers: { accept: 'application/json' } }),
    );
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
});

function context(overrides: Partial<ProjectContextPayloadV1>): ProjectContextPayloadV1 {
  return {
    status: 'ok',
    is_active: false,
    project: null,
    aliases: [],
    stores: [],
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
  it('measures both facts from an ok answer', () => {
    expect(
      registryReading(
        registryEnvelope(context({ is_active: true, project: project('Canonical') })),
      ),
    ).toEqual({ state: 'measured', label: 'Canonical', isActive: true });
  });

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

  it('reports an absent result as unknown rather than assuming', () => {
    expect(registryReading(undefined)).toEqual({ state: 'unknown' });
  });
});

describe('registryAnnotation', () => {
  it('says nothing about a name the registry confirmed', () => {
    expect(
      registryAnnotation(registryEnvelope(context({ project: project('Canonical') }))),
    ).toBeNull();
  });

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
