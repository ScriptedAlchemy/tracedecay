import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ProjectsPayloadSchema,
  type ProjectsPayload,
  type PublicCodeProject,
} from '../../contracts/wire.ts';
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
    stubRegistry(registryPayload('proj-real'));

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
    stubRegistry(registryPayload('proj-real'));

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
    stubRegistry(registryPayload('proj-real'));

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
   * Answered, and this id is not in it. The label has been contradicted rather
   * than left unconfirmed, so keeping it would state a name no authority backs
   * — but the id is still what every read routes by, so it stands in.
   */
  it('drops a label the registry contradicts, and says the project is not listed', async () => {
    useScope.getState().selectProject('proj-ghost', 'Looks Legitimate', 'unresolved');
    stubRegistry(registryPayload('proj-real'));

    const { findByText, queryByText } = render(queryWrapper(<ScopeBar />));

    expect(await findByText('proj-ghost')).toBeTruthy();
    expect(queryByText('Looks Legitimate')).toBeNull();
    const annotated = document.querySelector('[data-scope-label-annotation]');
    expect(annotated?.getAttribute('data-scope-label-annotation')).toBe('not in registry');
    // A project the registry does not list is certainly not the active one, so
    // this is a measured read-only rather than an unknown.
    const writability = scopeWritable(useScope.getState().scope);
    expect(writability.state).toBe('read_only');
    if (writability.state !== 'read_only') throw new Error('unreachable');
    expect(writability.reason).toContain('proj-ghost is not the active project');
  });

  it('resolves a deep-linked project to read-only when another project is active', async () => {
    useScope.getState().selectProject('proj-real', 'Canonical project', 'unresolved');
    stubRegistry(registryPayload('proj-other'));

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

/**
 * A registry body the daemon would actually send.
 *
 * Built through `ProjectsPayloadSchema` rather than hand-shaped: the stub this
 * replaced carried `status` and two fields per project, which the generated
 * contract rejects, so it only ever exercised the parse failure path while
 * appearing to test the success one.
 */
function registryPayload(activeProjectId: string): ProjectsPayload {
  const project = (projectId: string, label: string): PublicCodeProject => ({
    canonical_root: `/repos/${projectId}`,
    created_at: 1,
    default_branch: 'master',
    display_root: `~/repos/${projectId}`,
    git_common_dir: null,
    is_active: projectId === activeProjectId,
    label,
    last_seen_at: 2,
    project_id: projectId,
    project_root: `/repos/${projectId}`,
  });
  return ProjectsPayloadSchema.parse({
    active_project_id: activeProjectId,
    active_project_root: `/repos/${activeProjectId}`,
    error: null,
    limit: 100,
    project_tree: [],
    projects: [project('proj-real', 'Canonical project'), project('proj-other', 'Other project')],
    status: 'ok',
    summary: null,
    truncated: false,
  });
}

function stubRegistry(payload: ProjectsPayload) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue(
      new Response(JSON.stringify(payload), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    ),
  );
}
