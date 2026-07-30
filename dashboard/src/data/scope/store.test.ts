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
  reconciledLabel,
  scopeKey,
  scopeWritable,
  scopedUrl,
  useScope,
  type DashboardScope,
  type ProjectActivation,
  type RegistryReading,
} from './store.ts';

function project(projectId: string, activation: ProjectActivation): DashboardScope {
  return { kind: 'project', projectId, label: `label-${projectId}`, activation };
}

/** A measured registry answer. `projects` defaults to naming `activeProjectId`
 * so cases about activation do not have to restate the listing. */
function measured(
  activeProjectId: string | null,
  projects?: readonly { projectId: string; label: string }[],
): RegistryReading {
  return {
    state: 'measured',
    activeProjectId,
    projects:
      projects ??
      (activeProjectId ? [{ projectId: activeProjectId, label: `label-${activeProjectId}` }] : []),
  };
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
    expect(activationFor('proj_a', measured('proj_a'))).toBe('active');
  });

  it('resolves a different active project to selected', () => {
    expect(activationFor('proj_a', measured('proj_b'))).toBe('selected');
  });

  it('treats a measured absence of any active project as selected, not unknown', () => {
    // The registry answered: there is no active project. That is a real
    // reading and it does mean this project is not it.
    expect(activationFor('proj_a', measured(null))).toBe('selected');
  });

  it('keeps an unread registry unresolved rather than calling it not-active', () => {
    expect(activationFor('proj_a', { state: 'unknown' })).toBe('unresolved');
  });
});

/**
 * The label is not decoration. It reaches the sentence that names what a write
 * is about to affect, so a URL-supplied name that no authority backs is a
 * claim the dashboard should stop repeating the moment it can check it.
 */
describe('reconciledLabel', () => {
  it('replaces a stale deep-link label with the registry entry', () => {
    expect(
      reconciledLabel(
        'proj_a',
        'Old Name From A Bookmark',
        measured('proj_a', [{ projectId: 'proj_a', label: 'Canonical Name' }]),
      ),
    ).toBe('Canonical Name');
  });

  it('replaces a spoofed label on a project that is merely selected', () => {
    // Not the active project, so nothing here is writable — but the read-only
    // sentence still names the project, and it must not name it whatever the
    // link said.
    expect(
      reconciledLabel(
        'proj_b',
        'Production (definitely safe)',
        measured('proj_a', [
          { projectId: 'proj_a', label: 'Alpha' },
          { projectId: 'proj_b', label: 'Beta' },
        ]),
      ),
    ).toBe('Beta');
  });

  it('keeps the supplied label while the registry has not answered', () => {
    // Unknown authority is not licence to discard the only name available.
    expect(reconciledLabel('proj_a', 'From The Link', { state: 'unknown' })).toBe('From The Link');
  });

  it('falls back to the id when the registry answered and does not list it', () => {
    // Contradicted, not unconfirmed: keeping the claim would state a name no
    // authority backs.
    expect(
      reconciledLabel('proj_ghost', 'Looks Legitimate', measured('proj_a')),
    ).toBe('proj_ghost');
  });
});

describe('useScope.reconcileScope', () => {
  beforeEach(() => {
    useScope.setState({ scope: { kind: 'all' } });
  });

  it('promotes a deep-linked project once the registry names it active', () => {
    useScope.getState().selectProject('proj_a', 'alpha');
    expect(useScope.getState().scope).toMatchObject({ activation: 'unresolved' });
    useScope.getState().reconcileScope(measured('proj_a'));
    expect(useScope.getState().scope).toMatchObject({ activation: 'active' });
    expect(scopeWritable(useScope.getState().scope).state).toBe('writable');
  });

  it('names the write target from the registry, not from the link', () => {
    // The defect this closes: a link could choose the name that appears in
    // "Applies to …" for the project it is about to be written to.
    useScope.getState().selectProject('proj_a', 'Staging');
    useScope
      .getState()
      .reconcileScope(measured('proj_a', [{ projectId: 'proj_a', label: 'Production' }]));
    expect(useScope.getState().scope).toMatchObject({ label: 'Production' });
    expect(scopeWritable(useScope.getState().scope)).toEqual({
      state: 'writable',
      target: 'Production',
    });
  });

  it('corrects the label of a read-only selected project too', () => {
    useScope.getState().selectProject('proj_b', 'Spoofed');
    useScope
      .getState()
      .reconcileScope(
        measured('proj_a', [
          { projectId: 'proj_a', label: 'Alpha' },
          { projectId: 'proj_b', label: 'Beta' },
        ]),
      );
    const writability = scopeWritable(useScope.getState().scope);
    expect(writability.state).toBe('read_only');
    if (writability.state !== 'read_only') throw new Error('unreachable');
    expect(writability.reason).toContain('Beta is not the active project');
    expect(writability.reason).not.toContain('Spoofed');
  });

  it('returns a resolved scope to unresolved when the registry stops answering', () => {
    useScope.getState().selectProject('proj_a', 'alpha', 'active');
    useScope.getState().reconcileScope({ state: 'unknown' });
    // A registry that went unreadable does not leave the last good answer
    // standing: writability is no longer known, and a stale `active` here
    // would keep offering a write on the strength of a read that has since
    // stopped happening.
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
  });

  it('keeps the last known label when the registry stops answering', () => {
    // Losing the authority is not grounds for a correction in either
    // direction: the name stands, and it is writability that goes unknown.
    useScope.getState().selectProject('proj_a', 'From The Link');
    useScope
      .getState()
      .reconcileScope(measured('proj_a', [{ projectId: 'proj_a', label: 'Canonical' }]));
    useScope.getState().reconcileScope({ state: 'unknown' });
    expect(useScope.getState().scope).toMatchObject({
      label: 'Canonical',
      activation: 'unresolved',
    });
  });

  it('does not invent a correction from an unavailable registry', () => {
    useScope.getState().selectProject('proj_a', 'From The Link');
    useScope.getState().reconcileScope({ state: 'unknown' });
    expect(useScope.getState().scope).toMatchObject({
      label: 'From The Link',
      activation: 'unresolved',
    });
  });

  it('keeps the cache token stable across a label correction', () => {
    // The label is display text; the id is what selects rows. A correction
    // that re-keyed the caches would discard every read on the poll that
    // renamed the project.
    useScope.getState().selectProject('proj_a', 'Stale');
    const before = scopeKey(useScope.getState().scope);
    useScope
      .getState()
      .reconcileScope(measured('proj_a', [{ projectId: 'proj_a', label: 'Canonical' }]));
    expect(useScope.getState().scope).toMatchObject({ label: 'Canonical' });
    expect(scopeKey(useScope.getState().scope)).toBe(before);
  });

  it('holds the scope object identity when the reading changes nothing', () => {
    // This runs on a 30-second poll. A fresh object each time would re-render
    // every scope consumer in the shell for a reading that moved no fact.
    useScope.getState().selectProject('proj_a', 'Canonical');
    const reading = measured('proj_a', [{ projectId: 'proj_a', label: 'Canonical' }]);
    useScope.getState().reconcileScope(reading);
    const settled = useScope.getState().scope;
    useScope.getState().reconcileScope(reading);
    expect(useScope.getState().scope).toBe(settled);
  });

  it('leaves the all-projects scope alone', () => {
    useScope.getState().reconcileScope(measured('proj_a'));
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
