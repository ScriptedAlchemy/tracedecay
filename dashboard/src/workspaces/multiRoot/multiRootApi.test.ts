import { describe, expect, it } from 'vitest';
import {
  MULTI_ROOT_EXECUTE_ROUTE,
  MULTI_ROOT_SCOPE_SET_CAS_ROUTE,
  MULTI_ROOT_SCOPE_SET_READ_ROUTE,
} from './multiRootApi.ts';

describe('multi-root application routes', () => {
  it('matches the production dashboard mounts', () => {
    expect(MULTI_ROOT_SCOPE_SET_READ_ROUTE.path).toBe(
      '/api/application/multi-root/scope-set/read',
    );
    expect(MULTI_ROOT_SCOPE_SET_CAS_ROUTE.path).toBe(
      '/api/application/multi-root/scope-set/compare-and-swap',
    );
    expect(MULTI_ROOT_EXECUTE_ROUTE.path).toBe('/api/application/multi-root/execute');
  });

  it('rejects stale wire shapes before dispatch', () => {
    expect(
      MULTI_ROOT_EXECUTE_ROUTE.request.safeParse({
        scope_set_id: 'scope-set.dashboard',
        scope_set_revision: 0,
        scope_set_digest: `sha256:${'a'.repeat(64)}`,
        operation: { kind: 'query', request: {} },
        page: 0,
      }).success,
    ).toBe(false);
    expect(
      MULTI_ROOT_SCOPE_SET_READ_ROUTE.request.safeParse({
        scope_set_id: null,
      }).success,
    ).toBe(false);
  });
});
