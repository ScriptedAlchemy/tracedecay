import { callApplication, type WorkResult } from './workApi.ts';
import type { ApplicationRoute } from './workApi.ts';

/** Invoke one canonical Workflow operation through the dashboard HTTP adapter. */
export function callWorkflow<Request, Response>(
  route: ApplicationRoute<Request, Response>,
  request: Request,
  init?: RequestInit,
): Promise<WorkResult<Response>> {
  return callApplication(
    route,
    request,
    route.path,
    init,
    'the Workflow runtime is unavailable',
  );
}
