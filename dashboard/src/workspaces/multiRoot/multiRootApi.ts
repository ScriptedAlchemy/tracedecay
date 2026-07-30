import {
  AuthorizedScopeSetSchema,
  MultiRootExecuteRequestV1Schema,
  MultiRootQueryPageV1_for_AnyValueSchema,
  MultiRootScopeSetCasRequestV1Schema,
  MultiRootScopeSetCasResultV1Schema,
  MultiRootScopeSetReadRequestV1Schema,
} from '../../contracts/generated.ts';
import { callWork, type WorkResult, type WorkRoute } from '../work/workApi.ts';

export const MULTI_ROOT_SCOPE_SET_READ_ROUTE = {
  operation: 'operation.multi_root.scope_set_read',
  path: '/api/application/multi-root/scope-set/read',
  request: MultiRootScopeSetReadRequestV1Schema,
  response: AuthorizedScopeSetSchema.nullable(),
} as const satisfies WorkRoute<unknown, unknown>;

export const MULTI_ROOT_SCOPE_SET_CAS_ROUTE = {
  operation: 'operation.multi_root.scope_set_compare_and_swap',
  path: '/api/application/multi-root/scope-set/compare-and-swap',
  request: MultiRootScopeSetCasRequestV1Schema,
  response: MultiRootScopeSetCasResultV1Schema,
} as const satisfies WorkRoute<unknown, unknown>;

export const MULTI_ROOT_EXECUTE_ROUTE = {
  operation: 'operation.multi_root.execute',
  path: '/api/application/multi-root/execute',
  request: MultiRootExecuteRequestV1Schema,
  response: MultiRootQueryPageV1_for_AnyValueSchema,
} as const satisfies WorkRoute<unknown, unknown>;

export function callMultiRoot<Request, Response>(
  route: WorkRoute<Request, Response>,
  request: Request,
  init?: RequestInit,
): Promise<WorkResult<Response>> {
  return callWork(route, request, route.path, init);
}
