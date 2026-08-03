import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { BrainPage } from './BrainPage.tsx';
import { useScope } from '../../data/scope/store.ts';
import {
  ProjectsPayloadV1Schema,
} from '../../contracts/generated.ts';

/** Wire-true `GET /api/projects` body. `projects.rs::list` answers both failure
 * statuses with an explicit `"summary": null` / `"project_tree": null` — the
 * shape the two hand-written copies of this route both declared non-nullable,
 * so the exact responses `status` exists to distinguish were the ones that
 * failed to parse. */
function registryBody(status: string, error: string | null = null) {
  const ok = status === 'ok';
  return {
    status,
    error,
    limit: 100,
    truncated: ok ? false : null,
    active_project_id: null,
    active_project_root: '/repo',
    summary: ok ? { project_count: 0, repo_count: 0, truncated: false } : null,
    project_tree: ok ? [] : null,
    projects: ok ? [] : null,
  };
}

/**
 * The transport status each body really arrives with.
 *
 * `projects.rs::list` answers both registry failures with 503, never with a
 * 200 carrying a failing status — so serving these at 200, as this suite used
 * to, exercised a response the daemon cannot produce. It passed only because
 * `fetchLegacy` discarded non-2xx bodies, which is the defect that made these
 * very branches unreachable in production.
 *
 * An unrecognised status is different, and stays at 200: `status` is a bare
 * string in Rust, so a future successful state is a word this build has not
 * been taught, arriving on a perfectly ordinary success.
 */
function transportStatusFor(status: string): number {
  return status === 'missing_registry' || status === 'registry_unavailable' ? 503 : 200;
}

function renderBrain(status: string, error: string | null = null) {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(JSON.stringify(registryBody(status, error)), {
          status: transportStatusFor(status),
        }),
    ),
  );
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <BrainPage />
    </QueryClientProvider>,
  );
}

describe('BrainPage registry states', () => {
  afterEach(() => {
    useScope.getState().selectAllProjects();
    vi.unstubAllGlobals();
  });

  it('does not render a registry query failure as zero projects', async () => {
    renderBrain('registry_unavailable');

    expect(await screen.findByText(/registry read failed/i)).toBeTruthy();
    expect(screen.queryByText(/0 repositories · 0 projects/i)).toBeNull();
  });

  /**
   * `error` is the only field that says WHICH read failed. It is set on the
   * `registry_unavailable` responses and dropped on the floor by a surface that
   * rendered the status word alone, leaving every distinct failure of the
   * registry looking identical to the operator who has to fix one of them.
   */
  it("repeats the daemon's own account of a failed registry read", async () => {
    renderBrain('registry_unavailable', 'unable to open /home/x/.tracedecay/global.db');

    expect(await screen.findByText(/registry read failed/i)).toBeTruthy();
    expect(screen.getByText(/unable to open \/home\/x\/\.tracedecay\/global\.db/)).toBeTruthy();
  });

  it('says nothing extra when the daemon sent no reason', async () => {
    // `missing_registry` carries `error: None`, so there is nothing to repeat
    // and nothing is invented to fill the space.
    renderBrain('missing_registry');

    expect(await screen.findByText(/registry is not configured/i)).toBeTruthy();
    expect(screen.queryByText(/unable to open/i)).toBeNull();
  });

  // `projects.rs` writes `status` as a bare JSON string literal, so a fourth
  // value added there is not a malformed response — it is a response this
  // dashboard has not been taught yet. Rejecting it at the parser would take
  // the whole page down over a word, which is the failure Explorer shipped when
  // it typed `freshness` as a closed enum against a Rust `String`. So the
  // payload parses, and the surface names what it does not recognise instead of
  // guessing which known state it resembles.
  it('names an unrecognised registry status instead of failing the parse', async () => {
    expect(
      ProjectsPayloadV1Schema.safeParse({
        ...registryBody('ok'),
        status: 'mystery_success',
      }).success,
    ).toBe(true);

    renderBrain('mystery_success');
    expect(await screen.findByText(/unrecognised status: mystery_success/i)).toBeTruthy();
    expect(screen.queryByText(/repositories ·/i)).toBeNull();
    expect(screen.queryByText(/contains no projects/i)).toBeNull();
  });

  it('separates a missing registry from a successful empty registry', async () => {
    const missing = renderBrain('missing_registry');
    expect(await screen.findByText(/registry is not configured/i)).toBeTruthy();
    expect(screen.queryByText(/contains no projects/i)).toBeNull();
    missing.unmount();

    renderBrain('ok');
    expect(await screen.findByText(/registry contains no projects/i)).toBeTruthy();
    expect(screen.queryByText(/registry is not configured/i)).toBeNull();
  });
});
