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
import { describe, expect, it } from 'vitest';

import { targetedInvalidationKeys } from '../sse/useEvents.tsx';
import type { SseBatch } from '../sse/types.ts';
import {
  PROJECT_REGISTRY_ROOT,
  projectRegistryEntryKey,
  projectRegistryInvalidationKey,
  projectRegistryListKey,
  registryAnnotation,
  registryReading,
} from './projectRegistry.ts';
import type { ProjectContextPayloadV1 } from '../../contracts/generated.ts';

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
   * The heart of it. `useLegacy` appends the scope token to every key, so the
   * shapes below are what the client actually holds — and the invalidation has
   * to match all of them, including a per-project entry it cannot enumerate.
   */
  it('is reached by the registry invalidation, listing and entries alike', () => {
    const held = [
      [...projectRegistryListKey, 'all'],
      [...projectRegistryListKey, 'project:proj_a'],
      [...projectRegistryEntryKey('proj_a'), 'project:proj_a'],
      [...projectRegistryEntryKey('proj_b'), 'project:proj_b'],
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

    const invalidation = keys.find((key) => matches(key, [...projectRegistryListKey, 'all']));
    expect(invalidation).toBeDefined();
    expect(
      matches(invalidation as readonly string[], [
        ...projectRegistryEntryKey('proj_a'),
        'project:proj_a',
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
    const entryA = [...projectRegistryEntryKey('proj_a'), 'project:proj_a'];
    const entryB = [...projectRegistryEntryKey('proj_b'), 'project:proj_b'];
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

describe('registryReading', () => {
  it('measures both facts from an ok answer', () => {
    expect(
      registryReading({
        outcome: 'ok',
        data: context({ is_active: true, project: project('Canonical') }),
      }),
    ).toEqual({ state: 'measured', label: 'Canonical', isActive: true });
  });

  it('measures a not-active answer as a real reading', () => {
    expect(
      registryReading({
        outcome: 'ok',
        data: context({ is_active: false, project: project('Canonical') }),
      }),
    ).toEqual({ state: 'measured', label: 'Canonical', isActive: false });
  });

  it('carries a null through rather than reading it as a no', () => {
    // Both fields are nullable on the wire. Absent is "did not say".
    expect(
      registryReading({ outcome: 'ok', data: context({ is_active: null, project: null }) }),
    ).toEqual({ state: 'measured', label: null, isActive: null });
  });

  it('treats a non-ok status as no reading at all', () => {
    expect(
      registryReading({ outcome: 'ok', data: context({ status: 'missing_registry' }) }),
    ).toEqual({ state: 'unknown' });
  });

  /**
   * A registry that could not be read establishes nothing about the project.
   * Reporting a measurement for any of these would let a failed read discard a
   * label and withdraw a write; `unknown` keeps the name and says nothing it
   * has not established.
   */
  it.each([
    ['offline', { outcome: 'offline' as const }],
    ['unauthorized', { outcome: 'unauthorized' as const }],
    ['denied', { outcome: 'denied' as const }],
    ['error', { outcome: 'error' as const, detail: 'HTTP 500' }],
    ['unsupported_schema', { outcome: 'unsupported_schema' as const }],
  ])('reports %s as unknown', (_name, result) => {
    expect(registryReading(result)).toEqual({ state: 'unknown' });
  });

  /**
   * The two conditions the route reports with a status code, told apart. Both
   * used to arrive as `error` — the body was discarded — so a dead deep link
   * and a broken install were the same reading, and neither ever resolved.
   */
  it('measures a 404 not_found as the registry holding no such project', () => {
    expect(
      registryReading({
        outcome: 'unavailable',
        httpStatus: 404,
        status: 'not_found',
        reason: 'no project registered with id proj_ghost',
        data: context({ status: 'not_found', error: 'no project registered with id proj_ghost' }),
      }),
    ).toEqual({ state: 'absent', reason: 'no project registered with id proj_ghost' });
  });

  it.each(['missing_registry', 'registry_unavailable'])(
    'reports a 503 %s as unknown, not as an absent project',
    (status) => {
      expect(
        registryReading({
          outcome: 'unavailable',
          httpStatus: 503,
          status,
          reason: 'registry database could not be opened',
          data: context({ status }),
        }),
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
      registryAnnotation({ outcome: 'ok', data: context({ project: project('Canonical') }) }),
    ).toBeNull();
  });

  it('marks a name the answer did not confirm', () => {
    expect(registryAnnotation({ outcome: 'ok', data: context({ project: null }) })).toBe(
      'unconfirmed',
    );
  });

  it('says the read is still in flight rather than presenting the name as settled', () => {
    expect(registryAnnotation(undefined)).toBe('resolving');
  });

  it('names the transport state, and still says the name is unconfirmed', () => {
    expect(registryAnnotation({ outcome: 'offline' })).toBe('registry offline');
    expect(registryAnnotation({ outcome: 'error', detail: 'HTTP 500' })).toContain('unconfirmed');
  });

  it("repeats the registry's own sentence rather than restating the status code", () => {
    expect(
      registryAnnotation({
        outcome: 'unavailable',
        httpStatus: 404,
        status: 'not_found',
        reason: 'no project registered with id proj_ghost',
        data: context({ status: 'not_found' }),
      }),
    ).toBe('not in registry · no project registered with id proj_ghost');

    expect(
      registryAnnotation({
        outcome: 'unavailable',
        httpStatus: 503,
        status: 'missing_registry',
        reason: 'no registry at /home/x/.tracedecay/registry.db',
        data: context({ status: 'missing_registry' }),
      }),
    ).toBe('registry unavailable · no registry at /home/x/.tracedecay/registry.db');
  });

  it('still names the state when the payload sent no sentence', () => {
    expect(
      registryAnnotation({
        outcome: 'unavailable',
        httpStatus: 503,
        status: 'registry_unavailable',
        reason: null,
        data: context({ status: 'registry_unavailable' }),
      }),
    ).toBe('registry unavailable');
  });
});
