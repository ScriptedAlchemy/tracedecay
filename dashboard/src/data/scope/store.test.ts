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
  UNSCOPED_CACHE_KEY,
  activationFor,
  readOnlyScopeRefusal,
  reconciledLabel,
  requestScopeKey,
  scopedQueryKey,
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

/** The all-projects default. */
function all(): DashboardScope {
  return { kind: 'all' };
}

/** A measured answer about the selected project, as `GET /api/projects/{id}`
 * gives it: the canonical label and whether this is the active project. */
function measured(isActive: boolean | null, label: string | null = null): RegistryReading {
  return { state: 'measured', label, isActive };
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

  // The registry answered, and answered that there is nothing here. That is a
  // settled refusal, not a pending one: leaving it `unknown` would tell a
  // reader a dead link is still being checked for the rest of the session.
  it('reports a project the registry does not hold as refused, saying so', () => {
    const writability = scopeWritable(project('proj_ghost', 'absent'));
    expect(writability.state).toBe('read_only');
    if (writability.state !== 'read_only') throw new Error('unreachable');
    expect(writability.reason).toContain('proj_ghost');
    expect(writability.reason).toContain('no project');
    expect(writability.reason).toContain('switch to a registered project');
    // Not the pending-read sentence, which would be false here.
    expect(writability.reason).not.toContain('not known yet');
  });

  it('gives every scope a distinct answer across the cases', () => {
    const states = [
      scopeWritable({ kind: 'all' }).state,
      scopeWritable(project('proj_a', 'active')).state,
      scopeWritable(project('proj_b', 'selected')).state,
      scopeWritable(project('proj_c', 'unresolved')).state,
      scopeWritable(project('proj_ghost', 'absent')).state,
    ];
    expect(states).toEqual(['writable', 'writable', 'read_only', 'unknown', 'read_only']);
  });
});

describe('activationFor', () => {
  it('resolves the active project from a measured reading', () => {
    expect(activationFor(measured(true))).toBe('active');
  });

  it('resolves a measured not-active answer to selected', () => {
    // The registry answered about this exact project: it is not the active one.
    // That is a real reading and it does mean writes will be refused.
    expect(activationFor(measured(false))).toBe('selected');
  });

  it('keeps an answer that did not say unresolved rather than calling it not-active', () => {
    // `is_active` is nullable on the wire. Absent is not no: answering
    // `selected` would withdraw a write the gateway would have accepted.
    expect(activationFor(measured(null))).toBe('unresolved');
  });

  it('keeps an unread registry unresolved rather than calling it not-active', () => {
    expect(activationFor({ state: 'unknown' })).toBe('unresolved');
  });

  it('distinguishes a registry that holds no such project from one not yet read', () => {
    expect(activationFor({ state: 'absent', reason: 'no project registered' })).toBe('absent');
    expect(activationFor({ state: 'unknown' })).not.toBe('absent');
  });
});

/**
 * The label is not decoration. It reaches the sentence that names what a write
 * is about to affect, so a URL-supplied name that no authority backs is a
 * claim the dashboard should stop repeating the moment it can check it.
 */
describe('reconciledLabel', () => {
  it('replaces a stale deep-link label with the registry entry', () => {
    expect(reconciledLabel('Old Name From A Bookmark', measured(true, 'Canonical Name'))).toBe(
      'Canonical Name',
    );
  });

  it('replaces a spoofed label on a project that is merely selected', () => {
    // Not the active project, so nothing here is writable — but the read-only
    // sentence still names the project, and it must not name it whatever the
    // link said.
    expect(reconciledLabel('Production (definitely safe)', measured(false, 'Beta'))).toBe('Beta');
  });

  it('keeps the supplied label while the registry has not answered', () => {
    // Unknown authority is not licence to discard the only name available.
    expect(reconciledLabel('From The Link', { state: 'unknown' })).toBe('From The Link');
  });

  it('keeps the supplied label for a project the registry does not hold', () => {
    // A registry that holds no such project has no name to offer in its place,
    // and the id is not one. The name stays; `absent` activation is what says
    // it belongs to nothing.
    expect(reconciledLabel('From A Dead Link', { state: 'absent', reason: null })).toBe(
      'From A Dead Link',
    );
  });

  /**
   * The truncation defect, at the level of the function that had it. This used
   * to search the `/api/projects` listing and substitute the raw project id when
   * the id was not on the page — and the daemon truncates that listing to 100
   * entries by default, so a project past the end was indistinguishable from one
   * that does not exist. Nothing can express absence any more, so nothing can
   * assert it: an answer that named no project leaves the claim standing.
   */
  it('keeps the supplied label when the answer named no project', () => {
    expect(reconciledLabel('From The Link', measured(true, null))).toBe('From The Link');
    expect(reconciledLabel('From The Link', measured(false, null))).toBe('From The Link');
  });

  it('never substitutes an id for a label', () => {
    // A raw id in place of a name is itself a correction, and one no reading
    // here can support. It would also propagate to the address bar.
    const readings: RegistryReading[] = [
      measured(true, 'Canonical'),
      measured(false, 'Canonical'),
      measured(null, null),
      { state: 'unknown' },
      { state: 'absent', reason: 'no project registered with id proj_a' },
    ];
    for (const reading of readings) {
      expect(reconciledLabel('Supplied', reading)).not.toBe('proj_a');
    }
  });
});

describe('useScope.reconcileScope', () => {
  beforeEach(() => {
    useScope.setState({ scope: { kind: 'all' } });
  });

  it('promotes a deep-linked project once the registry names it active', () => {
    useScope.getState().selectProject('proj_a', 'alpha');
    expect(useScope.getState().scope).toMatchObject({ activation: 'unresolved' });
    useScope.getState().reconcileScope(measured(true, 'alpha'));
    expect(useScope.getState().scope).toMatchObject({ activation: 'active' });
    expect(scopeWritable(useScope.getState().scope).state).toBe('writable');
  });

  it('names the write target from the registry, not from the link', () => {
    // The defect this closes: a link could choose the name that appears in
    // "Applies to …" for the project it is about to be written to.
    useScope.getState().selectProject('proj_a', 'Staging');
    useScope.getState().reconcileScope(measured(true, 'Production'));
    expect(useScope.getState().scope).toMatchObject({ label: 'Production' });
    expect(scopeWritable(useScope.getState().scope)).toEqual({
      state: 'writable',
      target: 'Production',
    });
  });

  it('corrects the label of a read-only selected project too', () => {
    useScope.getState().selectProject('proj_b', 'Spoofed');
    useScope.getState().reconcileScope(measured(false, 'Beta'));
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
    useScope.getState().reconcileScope(measured(true, 'Canonical'));
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
    useScope.getState().reconcileScope(measured(true, 'Canonical'));
    expect(useScope.getState().scope).toMatchObject({ label: 'Canonical' });
    expect(scopeKey(useScope.getState().scope)).toBe(before);
  });

  it('holds the scope object identity when the reading changes nothing', () => {
    // This runs on a 30-second poll. A fresh object each time would re-render
    // every scope consumer in the shell for a reading that moved no fact.
    useScope.getState().selectProject('proj_a', 'Canonical');
    const reading = measured(true, 'Canonical');
    useScope.getState().reconcileScope(reading);
    const settled = useScope.getState().scope;
    useScope.getState().reconcileScope(reading);
    expect(useScope.getState().scope).toBe(settled);
  });

  it('leaves the all-projects scope alone', () => {
    useScope.getState().reconcileScope(measured(true, 'alpha'));
    expect(useScope.getState().scope).toEqual({ kind: 'all' });
  });

  /**
   * The selected project is past the end of the `/api/projects` page.
   *
   * The daemon truncates that listing (100 by default, 250 at most), so on a
   * profile with more projects than the page holds, a perfectly ordinary
   * selection is simply not in the response. Reconciliation used to search that
   * listing, so this case renamed the project to its raw id, announced "not in
   * registry", and — because the correction propagates — wrote the id into the
   * address bar. Both facts now come from the project's own route, which does
   * not have a page.
   */
  it('resolves a project the listing page never contained, when it is active', () => {
    useScope.getState().selectProject('proj_page_101', 'Stale Name');
    useScope.getState().reconcileScope(measured(true, 'Project One Hundred And One'));
    expect(useScope.getState().scope).toMatchObject({
      label: 'Project One Hundred And One',
      activation: 'active',
    });
    expect(scopeWritable(useScope.getState().scope)).toEqual({
      state: 'writable',
      target: 'Project One Hundred And One',
    });
  });

  it('resolves a project the listing page never contained, when it is not active', () => {
    useScope.getState().selectProject('proj_page_101', 'Stale Name');
    useScope.getState().reconcileScope(measured(false, 'Project One Hundred And One'));
    const writability = scopeWritable(useScope.getState().scope);
    expect(writability.state).toBe('read_only');
    if (writability.state !== 'read_only') throw new Error('unreachable');
    expect(writability.reason).toContain('Project One Hundred And One is not the active project');
    // Neither the stale claim nor a raw id.
    expect(writability.reason).not.toContain('Stale Name');
    expect(writability.reason).not.toContain('proj_page_101');
  });

  it('completes a reconciliation that first could not be made', () => {
    // The honest sequence: unknown while the read is failing or in flight, with
    // the supplied name standing and writability unknown, then settled when the
    // answer arrives. What must not happen is the first state asserting
    // anything about either fact.
    useScope.getState().selectProject('proj_page_101', 'Stale Name');
    useScope.getState().reconcileScope({ state: 'unknown' });
    expect(useScope.getState().scope).toMatchObject({
      label: 'Stale Name',
      activation: 'unresolved',
    });
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');

    useScope.getState().reconcileScope(measured(true, 'Canonical'));
    expect(useScope.getState().scope).toMatchObject({
      label: 'Canonical',
      activation: 'active',
    });
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
    expect(scopedUrl(project('proj_a', 'selected'), '/api/projects/proj_b')).toBe(
      '/api/projects/proj_b',
    );
  });
});

/**
 * The cache token for one request, which is a question about the ROUTE.
 *
 * This was first written as "did `scopedUrl` rewrite this request?", which
 * reads like the same question and is not. `scopedUrl` rewrites nothing under
 * the all-projects scope, so every route collapsed into the unscoped bucket
 * there and stopped agreeing with `scopeKey` — and `useSchedulerControl` writes
 * the reading it gets back to `scopeKey`. The result was a pause that took
 * effect on the daemon and never appeared on the button, because the fresh
 * reading went into a key no reader was watching.
 */
describe('requestScopeKey', () => {
  it('shares one token for a route the gateway never rewrites', () => {
    // The registry and the chrome: the same URL under every scope, so one entry.
    for (const url of ['/api/projects', '/api/dashboard']) {
      expect(requestScopeKey(all(), url)).toBe(UNSCOPED_CACHE_KEY);
      expect(requestScopeKey(project('proj_a', 'active'), url)).toBe(UNSCOPED_CACHE_KEY);
      expect(requestScopeKey(project('proj_b', 'selected'), url)).toBe(UNSCOPED_CACHE_KEY);
    }
  });

  it('keys a scoped route by scope, and agrees with scopeKey in every scope', () => {
    // Including `all`, which is the case that regressed: writers that compute
    // `scopeKey` and readers that compute this must land on the same entry.
    for (const scope of [all(), project('proj_a', 'active'), project('proj_b', 'selected')]) {
      expect(requestScopeKey(scope, '/api/automation/scheduler/status')).toBe(scopeKey(scope));
    }
  });

  it('separates two projects reading the same scoped route', () => {
    const a = requestScopeKey(project('proj_a', 'active'), '/api/observatory');
    const b = requestScopeKey(project('proj_b', 'active'), '/api/observatory');
    expect(a).not.toBe(b);
    expect(a).not.toBe(UNSCOPED_CACHE_KEY);
  });

  it('treats a non-API url as carrying no project', () => {
    expect(requestScopeKey(project('proj_a', 'active'), '/health')).toBe(UNSCOPED_CACHE_KEY);
  });
});

describe('scopedQueryKey', () => {
  it('shares daemon-wide registry entries across selected projects', () => {
    const key = ['projects', 'entry', 'proj_b'];
    expect(scopedQueryKey(project('proj_a', 'active'), key, '/api/projects/proj_b')).toEqual([
      ...key,
      UNSCOPED_CACHE_KEY,
    ]);
    expect(scopedQueryKey(project('proj_c', 'selected'), key, '/api/projects/proj_b')).toEqual([
      ...key,
      UNSCOPED_CACHE_KEY,
    ]);
  });

  it('keeps project-gateway reads isolated by their selected project', () => {
    const key = ['brain', 'graph-overview'];
    expect(
      scopedQueryKey(project('proj_a', 'active'), key, '/api/plugins/graph/overview'),
    ).not.toEqual(scopedQueryKey(project('proj_b', 'selected'), key, '/api/plugins/graph/overview'));
  });
});
