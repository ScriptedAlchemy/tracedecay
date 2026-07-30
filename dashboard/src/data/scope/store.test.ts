/**
 * The scope write authority.
 *
 * Every assertion here is a falsified-UI guard rather than a shape check. Two
 * failures are worth naming, because they are the ones this module exists to
 * prevent and neither one looks like a bug from the call site:
 *
 *   a scope whose writability is not known yet presenting as writable, which
 *   offers a control the gateway will refuse; and
 *
 *   an unread registry presenting as "not the active project", which withdraws
 *   a control that would have worked.
 *
 * So each case asserts the negative as well as the positive: never `writable`
 * when the answer is unknown, and never `read_only` when nothing was read.
 */
import { beforeEach, describe, expect, it } from 'vitest';

import {
  READ_ONLY_SCOPE_STATUS,
  activationFor,
  readOnlyScopeRefusal,
  scopeKey,
  scopeWritable,
  scopedUrl,
  useScope,
  type DashboardScope,
  type ProjectActivation,
} from './store.ts';

function project(projectId: string, activation: ProjectActivation): DashboardScope {
  return { kind: 'project', projectId, label: `label-${projectId}`, activation };
}

describe('scopeWritable', () => {
  it('reports the all-projects aggregate as writable against the active project', () => {
    const writability = scopeWritable({ kind: 'all' });
    expect(writability.state).toBe('writable');
    // Naming the target is the point: an unprefixed write reaches one project,
    // and a control that said only "writable" here would let the aggregate
    // view imply the change fans out across every project in it.
    expect(writability).toEqual({ state: 'writable', target: 'the active project' });
  });

  it('reports the active project as writable, named', () => {
    expect(scopeWritable(project('proj_a', 'active'))).toEqual({
      state: 'writable',
      target: 'label-proj_a',
    });
  });

  it('reports a selected non-active project as read-only with the remedy', () => {
    const writability = scopeWritable(project('proj_b', 'selected'));
    expect(writability.state).toBe('read_only');
    if (writability.state !== 'read_only') throw new Error('unreachable');
    expect(writability.reason).toContain('not the active project');
    // Actionable, not merely negative: the reason has to say what to do.
    expect(writability.reason).toContain('Switch scope');
  });

  // Guessing either way is a defect: `writable` offers a write that will be
  // refused, `read_only` withdraws one that would have worked.
  it('reports an unresolved deep-link scope as unknown, neither writable nor read-only', () => {
    const writability = scopeWritable(project('proj_c', 'unresolved'));
    expect(writability.state).toBe('unknown');
    if (writability.state !== 'unknown') throw new Error('unreachable');
    expect(writability.reason).toContain('not known yet');
  });

  it('gives every scope a distinct answer across the three cases', () => {
    const states = [
      scopeWritable({ kind: 'all' }).state,
      scopeWritable(project('proj_a', 'active')).state,
      scopeWritable(project('proj_b', 'selected')).state,
      scopeWritable(project('proj_c', 'unresolved')).state,
    ];
    expect(states).toEqual(['writable', 'writable', 'read_only', 'unknown']);
  });
});

describe('activationFor', () => {
  it('resolves the active project from a measured reading', () => {
    expect(activationFor('proj_a', { state: 'measured', activeProjectId: 'proj_a' })).toBe(
      'active',
    );
  });

  it('resolves a different active project to selected', () => {
    expect(activationFor('proj_a', { state: 'measured', activeProjectId: 'proj_b' })).toBe(
      'selected',
    );
  });

  it('treats a measured absence of any active project as selected, not unknown', () => {
    // The registry answered: there is no active project. That is a real
    // reading and it does mean this project is not it.
    expect(activationFor('proj_a', { state: 'measured', activeProjectId: null })).toBe('selected');
  });

  it('keeps an unread registry unresolved rather than calling it not-active', () => {
    expect(activationFor('proj_a', { state: 'unknown' })).toBe('unresolved');
  });
});

describe('useScope.resolveActivation', () => {
  beforeEach(() => {
    useScope.setState({ scope: { kind: 'all' } });
  });

  it('promotes a deep-linked project once the registry names it active', () => {
    useScope.getState().selectProject('proj_a', 'alpha');
    expect(useScope.getState().scope).toMatchObject({ activation: 'unresolved' });
    useScope.getState().resolveActivation({ state: 'measured', activeProjectId: 'proj_a' });
    expect(useScope.getState().scope).toMatchObject({ activation: 'active' });
    expect(scopeWritable(useScope.getState().scope).state).toBe('writable');
  });

  it('returns a resolved scope to unresolved when the registry stops answering', () => {
    useScope.getState().selectProject('proj_a', 'alpha', 'active');
    useScope.getState().resolveActivation({ state: 'unknown' });
    // A registry that went unreadable does not leave the last good answer
    // standing: writability is no longer known, and a stale `active` here
    // would keep offering a write on the strength of a read that has since
    // stopped happening.
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
  });

  it('leaves the all-projects scope alone', () => {
    useScope.getState().resolveActivation({ state: 'measured', activeProjectId: 'proj_a' });
    expect(useScope.getState().scope).toEqual({ kind: 'all' });
  });

  it('defaults a selection made without a registry reading to unresolved', () => {
    // The signature allows the caller to omit it, so the default is the state
    // this test pins: callers that have not read the registry cannot silently
    // produce a writable scope.
    useScope.getState().selectProject('proj_z', 'zeta');
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
  });
});

describe('readOnlyScopeRefusal', () => {
  const wireTrue = {
    status: READ_ONLY_SCOPE_STATUS,
    detail: 'project-scoped dashboard APIs are read-only for non-active projects',
    project_id: 'proj_b',
  };

  it('reads the gateway body the daemon actually sends', () => {
    expect(readOnlyScopeRefusal(wireTrue)).toEqual({
      projectId: 'proj_b',
      detail: 'project-scoped dashboard APIs are read-only for non-active projects',
    });
  });

  it('accepts extra keys the gateway may add later', () => {
    expect(readOnlyScopeRefusal({ ...wireTrue, retry_after: 30 })).not.toBeNull();
  });

  // Each of these is a 405 that is NOT this refusal. Returning a refusal for
  // any of them would attach a specific cause, and a specific remedy, to a
  // rejection that has neither.
  it.each([
    ['a different status', { ...wireTrue, status: 'not_found' }],
    ['no status at all', { detail: 'nope', project_id: 'proj_b' }],
    ['a missing project id', { status: READ_ONLY_SCOPE_STATUS, detail: 'nope' }],
    ['a missing detail', { status: READ_ONLY_SCOPE_STATUS, project_id: 'proj_b' }],
    ['a non-string detail', { ...wireTrue, detail: 42 }],
    ['an array body', [wireTrue]],
    ['a string body', 'read_only_project'],
    ['null', null],
    ['undefined', undefined],
  ])('rejects %s', (_name, body) => {
    expect(readOnlyScopeRefusal(body)).toBeNull();
  });
});

describe('scopedUrl and scopeKey', () => {
  it('keys the cache by project without folding in activation', () => {
    // Activation says what a write will do, not which rows come back. Folding
    // it in would throw away every cached read the moment it resolved.
    expect(scopeKey(project('proj_a', 'unresolved'))).toBe(scopeKey(project('proj_a', 'active')));
  });

  it('routes a selected project through the gateway and leaves the registry alone', () => {
    expect(scopedUrl(project('proj_a', 'selected'), '/api/observatory')).toBe(
      '/api/projects/proj_a/observatory',
    );
    expect(scopedUrl(project('proj_a', 'selected'), '/api/projects')).toBe('/api/projects');
  });
});
