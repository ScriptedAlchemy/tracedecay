/**
 * Which scopes the Work routes can answer for.
 *
 * This is the one place the Work surface can go wrong quietly. `/api/work` is
 * nested onto an application router built with the active project's id and is
 * absent from `project_api_router`, so the project gateway cannot serve it: a
 * `/api/projects/{id}/work/...` request is rewritten into a router with no such
 * path. Two failures follow from getting this wrong, and only one of them is
 * visible — routing every scope through the gateway makes the board 404 even
 * for the active project, and routing every scope unprefixed would show the
 * active project's tasks under a different project's name.
 */
import { describe, expect, it } from 'vitest';
import type { DashboardScope } from '../../data/scope/store.ts';

import { resumeCursor, workScopeAvailability } from './workQueries.ts';

function project(activation: 'active' | 'selected' | 'unresolved'): DashboardScope {
  return { kind: 'project', projectId: 'project.beta', label: 'Beta', activation };
}

describe('the scopes Work can answer for', () => {
  it('answers for the all-projects default and for the active project', () => {
    expect(workScopeAvailability({ kind: 'all' }).available).toBe(true);
    expect(workScopeAvailability(project('active')).available).toBe(true);
  });

  /** The defect this guards. A selected non-active project must not be sent to
   * these routes, because the answer would be the active project's board. */
  it('refuses a selected project, and names it', () => {
    const availability = workScopeAvailability(project('selected'));
    expect(availability.available).toBe(false);
    expect(availability.available === false && availability.detail).toContain('Beta');
    expect(availability.available === false && availability.detail).toContain('active project');
  });

  /** An unresolved activation is not a licence to guess. Reading it as active
   * would show the wrong project's work whenever the guess was wrong. */
  it('does not treat an unresolved activation as active', () => {
    expect(workScopeAvailability(project('unresolved')).available).toBe(false);
  });
});

describe('continuing a snapshot', () => {
  it('offers a cursor only where the daemon gave one', () => {
    expect(resumeCursor({ state: 'complete', returned: 2, total: 2 })).toBeUndefined();
    expect(
      resumeCursor({
        state: 'capped',
        cap: 1,
        returned: 1,
        total: 9,
        cursor: { generation_id: 'g', token: 'resume' },
        range: { start_exclusive: 0, end_inclusive: 1 },
      }),
    ).toEqual({ generation_id: 'g', token: 'resume' });
    expect(
      resumeCursor({
        state: 'partial',
        returned: 1,
        total: 9,
        cursor: { generation_id: 'g', token: 'resume-2' },
        range: { start_exclusive: 0, end_inclusive: 1 },
      }),
    ).toEqual({ generation_id: 'g', token: 'resume-2' });
  });
});
