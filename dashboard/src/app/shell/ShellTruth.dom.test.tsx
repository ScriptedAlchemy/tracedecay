import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ProjectContextPayloadV1Schema,
  ProjectsPayloadV1Schema,
  type ProjectContextPayloadV1,
  type ProjectsPayloadV1,
  type PublicCodeProject,
} from '../../contracts/generated.ts';
import { projectRegistryInvalidationKey } from '../../data/query/projectRegistry.ts';
import { scopeWritable, useScope } from '../../data/scope/store.ts';
import { DoctorInspector } from '../../workspaces/observatory/DoctorInspector.tsx';
import { NavRail } from './NavRail.tsx';
import { ScopeBar } from './ScopeBar.tsx';
import { StatusStrip } from './StatusStrip.tsx';

const eventState = vi.hoisted(() => ({ state: 'connecting' as const }));

vi.mock('../../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: eventState.state, lastEventAt: null }),
  useEventsConnection: () => null,
  // No connection is mounted here, so there is no projection to reconcile
  // against — the same reading the real hook gives for a null connection.
  useProjectionSync: () => ({ kind: 'unmounted' }) as const,
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
});

function queryWrapper(children: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: 0 } },
  });
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}

describe('shared shell truthfulness', () => {
  it('labels an unopened event stream as connecting, not synchronized', () => {
    const { getAllByRole, queryByText } = render(<StatusStrip />);

    // The strip reports the transport and the projection separately, so the
    // readings are collected rather than assumed to be one: what matters is that
    // neither of them claims synchronization for a stream that never opened.
    const readings = getAllByRole('status').map((node) => node.textContent);
    expect(readings).toContain('connecting');
    expect(readings.some((reading) => reading?.includes('sync'))).toBe(false);
    expect(queryByText('sync')).toBeNull();
  });

  it('does not render a receipt count when the event contract has no receipt family', () => {
    const { queryByText } = render(<StatusStrip />);

    expect(queryByText('Receipts')).toBeNull();
  });

  it('does not present the milestone name as the running build version', () => {
    const { queryByText } = render(<StatusStrip />);

    expect(queryByText('PR14')).toBeNull();
  });

  it('does not claim a local daemon when the backend is offline', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));

    const { queryByText } = render(queryWrapper(<MemoryRouter><NavRail /></MemoryRouter>));

    await waitFor(() => expect(queryByText('Local daemon')).toBeNull());
  });

  /**
   * The bar used to substitute the string `resolving` for the label until the
   * registry answered, which kept an unverified name off screen but also hid a
   * name the reader had just clicked in the palette — where the label came from
   * the registry to begin with — and left the bar the only surface with a view
   * on whether a name could be trusted, while the write-target prose used the
   * claim unqualified.
   *
   * So the unverified name is shown and marked unverified, then replaced. What
   * must not happen is the middle state presenting as settled.
   */
  it('marks an untrusted scope label unverified, then replaces it from the registry', async () => {
    useScope.getState().selectProject('proj-real', 'fabricated label');
    stubRegistry(entryPayload({ label: 'Canonical project', isActive: true }));

    const { queryByText, findByText } = render(queryWrapper(<ScopeBar />));

    expect(queryByText('fabricated label')).not.toBeNull();
    expect(document.querySelector('[data-scope-label-annotation]')?.textContent).toContain(
      'resolving',
    );

    expect(await findByText('Canonical project')).not.toBeNull();
    expect(queryByText('fabricated label')).toBeNull();
    // Settled: nothing qualifies a name the registry confirmed.
    expect(document.querySelector('[data-scope-label-annotation]')).toBeNull();
  });

  /**
   * The scope bar is the one place a selected project's activation is
   * reconciled, and `active_project_id` is the field that decides it. Every
   * other entry into a project scope — a deep link, the command palette —
   * arrives `unresolved`, and controls report writability as unknown until this
   * read lands. So a bar that renders the right label but never resolves would
   * leave every write in the product permanently disabled with the wrong
   * reason.
   */
  it('resolves a deep-linked project to active when the registry names it active', async () => {
    useScope.getState().selectProject('proj-real', 'Canonical project', 'unresolved');
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
    stubRegistry(entryPayload({ label: 'Canonical project', isActive: true }));

    render(queryWrapper(<ScopeBar />));

    await waitFor(() => expect(useScope.getState().scope).toMatchObject({ activation: 'active' }));
    const writability = scopeWritable(useScope.getState().scope);
    expect(writability).toEqual({ state: 'writable', target: 'Canonical project' });
  });

  /**
   * Reconciliation settles the label as well as the activation, so the write
   * target is named by the registry rather than by whatever put the scope
   * there. Before this, a deep link carrying a wrong label produced a
   * correctly-enabled control that named the project wrongly — the bar showed
   * the canonical name while "Applies to …" showed the link's claim, which is
   * the same fact in two places disagreeing.
   */
  it('names its write target from the registry, not from the label it was selected with', async () => {
    useScope.getState().selectProject('proj-real', 'fabricated label', 'unresolved');
    stubRegistry(entryPayload({ label: 'Canonical project', isActive: true }));

    const { findByText, queryByText } = render(queryWrapper(<ScopeBar />));

    expect(await findByText('Canonical project')).toBeTruthy();
    await waitFor(() => expect(useScope.getState().scope).toMatchObject({ activation: 'active' }));
    expect(scopeWritable(useScope.getState().scope)).toEqual({
      state: 'writable',
      target: 'Canonical project',
    });
    // Not merely outvoted — gone. The claim is not left anywhere on the bar.
    expect(queryByText('fabricated label')).toBeNull();
  });

  /**
   * The one case where the URL's label is kept: the registry did not answer, so
   * there is nothing to correct it with. Discarding it here would replace a
   * name that may well be right with an opaque id, on no evidence — the same
   * mistake as the correction, pointed the other way.
   */
  it('keeps the selected label, unconfirmed, when the registry cannot be read', async () => {
    useScope.getState().selectProject('proj-real', 'name from the link', 'unresolved');
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));

    const { findByText } = render(queryWrapper(<ScopeBar />));

    // Shown, and shown as unconfirmed: the annotation is what keeps this from
    // reading as a registry-backed name.
    expect(await findByText('name from the link')).toBeTruthy();
    const annotated = await waitFor(() => {
      const found = document.querySelector('[data-scope-label-annotation]');
      expect(found).not.toBeNull();
      return found as HTMLElement;
    });
    expect(annotated.getAttribute('data-scope-label-annotation')).toBe('registry offline');
    expect(useScope.getState().scope).toMatchObject({
      label: 'name from the link',
      activation: 'unresolved',
    });
    // And unknown authority did not become read-only.
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
  });

  /**
   * The truncation defect, at the surface.
   *
   * Reconciliation used to search `/api/projects`, which the daemon truncates to
   * a page — 100 entries by default. A selected project past the end of that
   * page produced exactly what a nonexistent project produced, so the bar
   * renamed a real project to its raw id and announced "not in registry". This
   * asks the project's own route instead, which has no page, so a listing that
   * omits the id establishes nothing and cannot be consulted for it.
   *
   * The stub answers the listing with a truncated page that excludes this
   * project, precisely so a regression to searching it fails here.
   */
  it('resolves a project the truncated listing omits, and never renames it to its id', async () => {
    useScope.getState().selectProject('proj-page-101', 'Stale Bookmark Name', 'unresolved');
    stubRoutes({
      '/api/projects': truncatedListing(),
      '/api/projects/proj-page-101': entryPayload({
        label: 'Project One Hundred And One',
        isActive: true,
      }),
    });

    const { findByText, queryByText } = render(queryWrapper(<ScopeBar />));

    expect(await findByText('Project One Hundred And One')).toBeTruthy();
    expect(queryByText('Stale Bookmark Name')).toBeNull();
    // Not renamed to the id, and not annotated as missing.
    expect(queryByText('proj-page-101')).toBeNull();
    expect(document.querySelector('[data-scope-label-annotation]')).toBeNull();
    expect(scopeWritable(useScope.getState().scope)).toEqual({
      state: 'writable',
      target: 'Project One Hundred And One',
    });
    // The stronger statement, and the one that keeps the defect from returning:
    // the page was never even asked for. A reconciliation that searched a
    // listing would have to read one.
    expect(requestedUrls).toContain('/api/projects/proj-page-101');
    expect(requestedUrls).not.toContain('/api/projects');
  });

  /**
   * The two ways a deep link fails to resolve, which the route reports with two
   * different status codes and which the dashboard must not merge.
   *
   * They used to be one case here, because `fetchLegacy` discarded every
   * non-2xx body: a dead link and a registry that could not be opened both
   * arrived as `HTTP 404`/`HTTP 503` with nothing to tell them apart, so the
   * only honest reading available was "unconfirmed" for both. That left a stale
   * bookmark resolving forever — the reader was told the check was still
   * pending long after it had come back with an answer.
   *
   * Now the body is carried, so each says what it is. What must not change is
   * the label: neither reading renames the project to its id, because neither
   * one carries a name to replace it with.
   */
  it('reports a project the registry does not hold as absent, keeping its name', async () => {
    useScope.getState().selectProject('proj-ghost', 'Looks Legitimate', 'unresolved');
    stubRoutes({
      '/api/projects/proj-ghost': {
        status: 404,
        body: failurePayload('not_found', 'no project registered with id proj-ghost'),
      },
    });

    const { findByText, queryByText } = render(queryWrapper(<ScopeBar />));

    expect(await findByText('Looks Legitimate')).toBeTruthy();
    const annotated = await waitFor(() => {
      const found = document.querySelector('[data-scope-label-annotation]');
      expect(found).not.toBeNull();
      return found as HTMLElement;
    });
    // The registry's own discriminant and sentence, not a status code.
    const annotation = annotated.getAttribute('data-scope-label-annotation') ?? '';
    expect(annotation).toContain('not in registry');
    expect(annotation).toContain('no project registered with id proj-ghost');
    // Never renamed to the raw id, which is a correction no reading supports.
    expect(queryByText('proj-ghost')).toBeNull();

    // A settled refusal rather than a pending one: there is nothing here to
    // write to, and saying "not known yet" would be false once the answer is in.
    await waitFor(() =>
      expect(useScope.getState().scope).toMatchObject({ activation: 'absent' }),
    );
    const writability = scopeWritable(useScope.getState().scope);
    expect(writability.state).toBe('read_only');
    if (writability.state !== 'read_only') throw new Error('unreachable');
    expect(writability.reason).toContain('proj-ghost');
    expect(writability.reason).not.toContain('not known yet');
  });

  it('keeps an unattributable 404 unconfirmed rather than asserting absence', async () => {
    useScope.getState().selectProject('proj-ghost', 'Looks Legitimate', 'unresolved');
    stubRoutes({
      '/api/projects/proj-ghost': { status: 404, body: { detail: 'nginx: not found' } },
    });

    const { findByText } = render(queryWrapper(<ScopeBar />));

    expect(await findByText('Looks Legitimate')).toBeTruthy();
    const annotated = await waitFor(() => {
      const found = document.querySelector('[data-scope-label-annotation]');
      expect(found).not.toBeNull();
      return found as HTMLElement;
    });
    expect(annotated.getAttribute('data-scope-label-annotation')).toContain('unconfirmed');
    expect(useScope.getState().scope).toMatchObject({ activation: 'unresolved' });
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
  });

  it('keeps a project unconfirmed when the registry itself is unavailable', async () => {
    // 503, not 404: the registry could not be read, so it established nothing
    // about this project. Reporting absence here would discard a good label and
    // refuse a write that a working registry would have accepted.
    useScope.getState().selectProject('proj-real', 'Looks Legitimate', 'unresolved');
    stubRoutes({
      '/api/projects/proj-real': {
        status: 503,
        body: failurePayload('registry_unavailable', 'registry database could not be opened'),
      },
    });

    const { findByText, queryByText } = render(queryWrapper(<ScopeBar />));

    expect(await findByText('Looks Legitimate')).toBeTruthy();
    const annotated = await waitFor(() => {
      const found = document.querySelector('[data-scope-label-annotation]');
      expect(found).not.toBeNull();
      return found as HTMLElement;
    });
    const annotation = annotated.getAttribute('data-scope-label-annotation') ?? '';
    expect(annotation).toContain('registry unavailable');
    expect(annotation).toContain('registry database could not be opened');
    expect(annotation).not.toContain('not in registry');
    expect(queryByText('proj-real')).toBeNull();

    // Unknown, not read-only: the dashboard has no answer to refuse a write on.
    expect(useScope.getState().scope).toMatchObject({ activation: 'unresolved' });
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
  });

  /**
   * F1: the scope bar's registry read had a private query key that no event
   * named, and no poll. It is where activation is reconciled, so a rename or an
   * active-project switch left every write control in the product acting on the
   * pre-change answer for the rest of the session.
   *
   * The invalidation key here is the one the SSE handler emits — see
   * `projectRegistry.test.ts`, which pins the handler's output to this same
   * constant — so this covers the second half of the link: that the key reaches
   * this query and reconciliation runs again.
   */
  it('re-reconciles activation and label when a registry change is invalidated', async () => {
    useScope.getState().selectProject('proj-real', 'Before Rename', 'unresolved');
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: 0 } },
    });
    stubRegistry(entryPayload({ label: 'Before Rename', isActive: true }));

    const { findByText } = render(
      <QueryClientProvider client={client}>
        <ScopeBar />
      </QueryClientProvider>,
    );
    await findByText('Before Rename');
    await waitFor(() => expect(useScope.getState().scope).toMatchObject({ activation: 'active' }));

    // The daemon renamed it and made another project active.
    stubRegistry(entryPayload({ label: 'After Rename', isActive: false }));
    await client.invalidateQueries({ queryKey: [...projectRegistryInvalidationKey] });

    expect(await findByText('After Rename')).toBeTruthy();
    await waitFor(() =>
      expect(useScope.getState().scope).toMatchObject({
        label: 'After Rename',
        activation: 'selected',
      }),
    );
    const writability = scopeWritable(useScope.getState().scope);
    expect(writability.state).toBe('read_only');
    if (writability.state !== 'read_only') throw new Error('unreachable');
    expect(writability.reason).toContain('After Rename is not the active project');
  });

  it('resolves a deep-linked project to read-only when another project is active', async () => {
    useScope.getState().selectProject('proj-real', 'Canonical project', 'unresolved');
    stubRegistry(entryPayload({ label: 'Canonical project', isActive: false }));

    render(queryWrapper(<ScopeBar />));

    await waitFor(() =>
      expect(useScope.getState().scope).toMatchObject({ activation: 'selected' }),
    );
    const writability = scopeWritable(useScope.getState().scope);
    expect(writability.state).toBe('read_only');
    if (writability.state !== 'read_only') throw new Error('unreachable');
    // The remedy has to be in the sentence: a disabled control whose reason does
    // not say how to enable it is indistinguishable from a broken one.
    expect(writability.reason).toContain('not the active project');
    expect(writability.reason).toContain('Switch scope');
  });

  /**
   * A registry that could not be read establishes nothing about which project
   * is active. Resolving it to `selected` would be the same defect this project
   * forbids everywhere else — an unread source rendering as a measurement —
   * and it would tell the reader their project is not the active one on no
   * evidence at all.
   */
  it('leaves activation unresolved when the registry cannot be read', async () => {
    useScope.getState().selectProject('proj-real', 'Canonical project', 'unresolved');
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));

    render(queryWrapper(<ScopeBar />));

    // Wait for the read to have resolved into a rendered reading, so the
    // assertion below is about a reconciliation that ran rather than one that
    // had not happened yet.
    await waitFor(() => expect(screen.getByText(/registry offline/i)).toBeTruthy());
    expect(useScope.getState().scope).toMatchObject({ activation: 'unresolved' });
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
  });

  /**
   * The rail's health dot and the Doctor panel are the app's two Doctor
   * readings, and they were taken in different scopes: the dot went through the
   * project gateway while the panel asked for `/api/doctor/findings`
   * unprefixed, which the daemon answers for whichever project *it* has active.
   * Selecting a project therefore put one project's diagnosis in the panel and
   * another's in the rail beside it, with nothing on screen saying so.
   */
  it('reads the rail health dot and the Doctor panel in the same scope', async () => {
    useScope.getState().selectProject('proj-real', 'Canonical project', 'active');
    const requested: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        requested.push(typeof input === 'string' ? input : String(input));
        return new Response('{}', { status: 503 });
      }),
    );

    render(queryWrapper(<MemoryRouter><NavRail /></MemoryRouter>));
    render(queryWrapper(<DoctorInspector />));

    await waitFor(() => {
      expect(requested.some((url) => url.includes('/storage/findings'))).toBe(true);
      expect(requested.some((url) => url.includes('/doctor/findings'))).toBe(true);
    });
    const unscoped = requested.filter((url) => !url.startsWith('/api/projects/proj-real/'));
    expect(unscoped).toEqual([]);
  });
});

function projectRecord(projectId: string, label: string): PublicCodeProject {
  return {
    canonical_root: `/repos/${projectId}`,
    created_at: 1,
    default_branch: 'master',
    display_root: `~/repos/${projectId}`,
    git_common_dir: null,
    label,
    last_seen_at: 2,
    project_id: projectId,
    project_root: `/repos/${projectId}`,
  };
}

/**
 * A `GET /api/projects/{id}` body the daemon would actually send.
 *
 * Parsed through the generated schema rather than hand-shaped, so a body this
 * dashboard could not read cannot masquerade as a successful reading — the stub
 * that preceded these carried fields the contract rejects, and so only ever
 * exercised the parse-failure path while appearing to test the success one.
 */
function entryPayload({
  label,
  isActive,
}: {
  label: string;
  isActive: boolean;
}): ProjectContextPayloadV1 {
  return ProjectContextPayloadV1Schema.parse({
    status: 'ok',
    error: null,
    is_active: isActive,
    project: projectRecord('proj-real', label),
    aliases: [],
    stores: [],
  });
}

/**
 * The failure bodies `GET /api/projects/{id}` really sends, parsed through the
 * generated schema so a contract change breaks the fixture rather than being
 * absorbed by it.
 *
 * Copied field for field from `src/dashboard/projects.rs`: both are complete
 * `ProjectContextPayloadV1` values carrying a non-`ok` status, not the bare
 * `{status}` stub they were once written as. The distinction is the whole
 * point of these cases — a hand-shortened body fails schema validation and
 * arrives as a build mismatch, which is a different reading from the one the
 * daemon is actually reporting.
 */
function failurePayload(status: string, error: string | null = null): ProjectContextPayloadV1 {
  return ProjectContextPayloadV1Schema.parse({
    status,
    error,
    is_active: null,
    project: null,
    aliases: [],
    stores: [],
  });
}

/**
 * A listing that is truncated and does not contain the selected project.
 *
 * The shape the daemon sends for a profile with more projects than the page
 * holds — `truncated: true`, `limit` entries, and any project past the end
 * simply missing. Reconciliation must not consult this at all, which the test
 * using it asserts directly by checking the route is never requested.
 */
function truncatedListing(): ProjectsPayloadV1 {
  return ProjectsPayloadV1Schema.parse({
    active_project_id: 'proj-first',
    active_project_root: '/repos/proj-first',
    error: null,
    limit: 2,
    project_tree: [],
    projects: [
      projectRecord('proj-first', 'First page project'),
      projectRecord('proj-second', 'Second page project'),
    ],
    status: 'ok',
    summary: null,
    truncated: true,
  });
}

/** Every request, in order, so a test can assert what was *not* asked for. */
let requestedUrls: string[] = [];

function stubRegistry(payload: ProjectContextPayloadV1) {
  requestedUrls = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      requestedUrls.push(String(input));
      return jsonResponse(200, payload);
    }),
  );
}

/** Route-aware stub. An unmapped route answers 404, so a test that forgot to map
 * something sees a missing route rather than another route's body. */
function stubRoutes(routes: Record<string, unknown | { status: number; body: unknown }>) {
  requestedUrls = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      requestedUrls.push(url);
      const match = routes[url.split('?')[0] ?? url];
      if (match === undefined) return jsonResponse(404, failurePayload('not_found'));
      const framed = match as { status?: number; body?: unknown };
      return typeof framed.status === 'number'
        ? jsonResponse(framed.status, framed.body)
        : jsonResponse(200, match);
    }),
  );
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}
