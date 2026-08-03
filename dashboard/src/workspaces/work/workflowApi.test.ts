import { afterEach, describe, expect, it, vi } from 'vitest';
import { callWorkflow } from './workflowApi.ts';
import {
  WORKFLOW_ACTIVATE_DEFINITION_ROUTE,
  WORKFLOW_ROUTES,
} from './workflowRoutes.ts';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Workflow application bindings', () => {
  it('binds all five canonical operations to the shared application router', () => {
    expect(
      WORKFLOW_ROUTES.map(({ operation, path }) => ({ operation, path })),
    ).toEqual([
      {
        operation: 'operation.workflow.register_definition',
        path: '/api/application/workflow/register-definition',
      },
      {
        operation: 'operation.workflow.activate_definition',
        path: '/api/application/workflow/activate-definition',
      },
      {
        operation: 'operation.workflow.execute_fan_out',
        path: '/api/application/workflow/execute-fan-out',
      },
      {
        operation: 'operation.workflow.handoff_issue',
        path: '/api/application/workflow/handoff-issue',
      },
      {
        operation: 'operation.workflow.handoff_redeem',
        path: '/api/application/workflow/handoff-redeem',
      },
    ]);
  });

  it('validates and calls the canonical handler path', async () => {
    const fetch = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          kind: 'success',
          value: {
            outcome: {
              outcome: 'effect',
              value: {
                payload: {
                  definition_id: 'workflow.dashboard',
                  active_version: 2,
                },
              },
            },
          },
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
    );
    vi.stubGlobal('fetch', fetch);

    const result = await callWorkflow(WORKFLOW_ACTIVATE_DEFINITION_ROUTE, {
      definition_id: 'workflow.dashboard',
      expected_active_version: 1,
      replacement_version: 2,
    });

    expect(result).toEqual({
      outcome: 'value',
      value: {
        definition_id: 'workflow.dashboard',
        active_version: 2,
      },
    });
    expect(fetch).toHaveBeenCalledWith(
      '/api/application/workflow/activate-definition',
      expect.objectContaining({ method: 'POST' }),
    );
  });

  it('refuses malformed input before transport', async () => {
    const fetch = vi.fn();
    vi.stubGlobal('fetch', fetch);

    const result = await callWorkflow(
      WORKFLOW_ACTIVATE_DEFINITION_ROUTE,
      {
        definition_id: 'workflow.dashboard',
        expected_active_version: 1,
      } as never,
    );

    expect(result).toEqual({
      outcome: 'refused',
      state: 'error',
      detail: 'the request does not satisfy operation.workflow.activate_definition',
    });
    expect(fetch).not.toHaveBeenCalled();
  });

  it('reports Workflow unavailability without relabelling it as Work', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(new Response(null, { status: 503 })),
    );

    const result = await callWorkflow(WORKFLOW_ACTIVATE_DEFINITION_ROUTE, {
      definition_id: 'workflow.dashboard',
      expected_active_version: 1,
      replacement_version: 2,
    });

    expect(result).toEqual({
      outcome: 'refused',
      state: 'unavailable',
      detail: 'the Workflow runtime is unavailable',
    });
  });
});
