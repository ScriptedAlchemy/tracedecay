import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter, useLocation } from 'react-router';
import type { DeliveryInboxV1 } from '../../contracts/generated.ts';
import type { FeedbackProximityReadResultV1 } from '../../contracts/index.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { DeliveryPage } from './DeliveryPage.tsx';

const INBOX = {
  registry_state: 'ready',
  projects: [
    {
      project_id: 'project.alpha',
      label: 'alpha',
      project_root: '/src/alpha',
      git_common_dir: '/src/alpha/.git',
      repository_id: 'repository.alpha',
      worktree_id: 'worktree.alpha',
      branch_ref: 'refs/heads/feature/delivery',
      indexed_head_commit_id: 'a'.repeat(40),
      indexed_generation: 'generation.alpha.1',
      provider_state: 'ready',
    },
  ],
  pull_requests: [
    {
      id: 'project.alpha:github:42',
      project_id: 'project.alpha',
      repository_id: 'repository.alpha',
      worktree_id: 'worktree.alpha',
      branch_ref: 'refs/heads/feature/delivery',
      indexed_head_commit_id: 'a'.repeat(40),
      indexed_generation: 'generation.alpha.1',
      state: 'current',
      pull_request: {
        id: 'github:42',
        label: 'Pull request #42 — Admit delivery inbox',
        provider: 'github',
        pull_request_id: '42',
        identity: {
          title: 'Admit delivery inbox',
          state: 'open',
          draft: false,
          additions: 120,
          deletions: 35,
          changed_files: 8,
        },
        operations: [],
      },
      attention: [
        {
          id: 'project.alpha:42:ci_failure',
          project_id: 'project.alpha',
          pull_request_id: '42',
          source: 'ci_failure',
          state: 'active',
          evidence: [{ kind: 'ci_failure', failure_anchor: 'anchor.ci.42' }],
          coverage: 'complete',
          observed_at_micros: 1_700_000_000_000_000,
        },
        {
          id: 'project.alpha:42:overlapping_edit',
          project_id: 'project.alpha',
          pull_request_id: '42',
          source: 'overlapping_edit',
          state: 'unavailable',
          evidence: [],
          coverage: 'unsupported',
          observed_at_micros: null,
        },
      ],
      shared_code: [
        {
          kind: 'shared_code',
          state: 'requires_selection',
          href: '/code?view=shared-code',
          source_generation: 'generation.alpha.1',
        },
        {
          kind: 'compare',
          state: 'requires_selection',
          href: '/code?view=compare',
          source_generation: 'generation.alpha.1',
        },
      ],
    },
  ],
  membership_edges: [
    {
      id: 'project.alpha:42:branch_pull_request_reference',
      project_id: 'project.alpha',
      pull_request_id: '42',
      basis: {
        kind: 'branch_pull_request_reference',
        branch_ref: 'refs/heads/feature/delivery',
        head_commit_id: 'a'.repeat(40),
      },
    },
  ],
  omitted_projects: 0,
  excluded_pull_requests: 1,
} satisfies DeliveryInboxV1;

function LocationProbe() {
  return <output data-testid="location">{useLocation().search}</output>;
}

/** Wraps a Work-route payload in the application envelope `callWork` walks
 * (`crates/tracedecay-api/src/lib.rs`'s `HttpJsonEnvelope`), the same shape
 * `LoomPage.dom.test.tsx` uses for `/api/feedback/proximity`. */
function applicationEnvelope(payload: unknown) {
  return {
    kind: 'success',
    value: {
      binding_id: 'binding.dashboard.feedback_proximity.v1',
      contract: { schema_id: 'schema.application.feedback.proximity.result', schema_revision: 1 },
      request_id: 'request-proximity',
      scope: {
        project_id: 'project.alpha',
        repository_id: 'repository.alpha',
        worktree_id: 'worktree.alpha',
        reference: 'refs/heads/feature/delivery',
        scope_digest: 'sha256:scope',
      },
      outcome: { outcome: 'evidence', value: { payload } },
    },
  };
}

const PROXIMITY_UNAVAILABLE_BODY = applicationEnvelope({
  state: 'unavailable',
  observed_at: 1_700_000_000_000_000,
} satisfies FeedbackProximityReadResultV1);

function serveRoutes(routes: Record<string, { status: number; body: unknown }>) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    const hit = Object.entries(routes).find(([path]) => url.includes(path));
    const { status, body } = hit?.[1] ?? { status: 404, body: { status: 'not_found' } };
    return {
      ok: status >= 200 && status < 300,
      status,
      json: async () => body,
    } as Response;
  });
}

function renderDelivery(
  payload: DeliveryInboxV1,
  domainState = 'ready',
  route = '/delivery',
  proximityBody: unknown = PROXIMITY_UNAVAILABLE_BODY,
) {
  vi.stubGlobal(
    'fetch',
    serveRoutes({
      '/api/delivery/inbox': { status: 200, body: fixtureEnvelope(payload, domainState) },
      '/api/feedback/proximity': { status: 200, body: proximityBody },
    }),
  );
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[route]}>
        <DeliveryPage />
        <LocationProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('DeliveryPage', () => {
  it('renders only admitted pull requests and explains membership', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX);

    await user.click(await screen.findByRole('button', { name: /Admit delivery inbox/ }));

    expect(screen.queryByText('Unrelated provider PR')).toBeNull();
    expect(screen.getByText('Branch and pull request reference')).toBeTruthy();
    expect(screen.getAllByText(/refs\/heads\/feature\/delivery/).length).toBeGreaterThan(0);
    expect(screen.getByText(/1 unrelated provider pull request excluded/)).toBeTruthy();
    expect(screen.getByTestId('location').textContent).toContain('pr=project.alpha%3Agithub%3A42');
  });

  it('restores project, pull request, attention source, and evidence from the URL', async () => {
    renderDelivery(
      INBOX,
      'ready',
      '/delivery?project=project.alpha&pr=project.alpha%3Agithub%3A42&attention=ci_failure&evidence=anchor.ci.42',
    );

    const detail = await screen.findByRole('region', { name: 'Pull request detail' });
    expect(within(detail).getByText('CI failure')).toBeTruthy();
    expect(within(detail).getByText('anchor.ci.42')).toBeTruthy();
    expect(within(detail).getByText('Overlapping edit')).toBeTruthy();
  });

  it('links to the existing verified Code views', async () => {
    renderDelivery(INBOX);

    const shared = await screen.findByRole('link', { name: 'Open Shared Code' });
    const compare = screen.getByRole('link', { name: 'Open Compare' });
    expect(shared.getAttribute('href')).toBe('/code?view=shared-code');
    expect(compare.getAttribute('href')).toBe('/code?view=compare');
  });

  it('renders provider-not-configured as a typed zero, not a transport failure', async () => {
    renderDelivery({
      ...INBOX,
      projects: [{ ...INBOX.projects[0]!, provider_state: 'not_configured' }],
      pull_requests: [],
      membership_edges: [],
      excluded_pull_requests: 0,
    });

    expect(await screen.findByText('Provider not configured')).toBeTruthy();
    expect(screen.getByText('No admitted pull requests')).toBeTruthy();
    expect(screen.queryByText(/transport/i)).toBeNull();
  });

  it('renders an unavailable registry distinctly from a complete zero inbox', async () => {
    renderDelivery(
      {
        registry_state: 'unavailable',
        projects: [],
        pull_requests: [],
        membership_edges: [],
        omitted_projects: 0,
        excluded_pull_requests: 0,
      },
      'unknown',
    );

    expect(await screen.findByText('Project registry unavailable')).toBeTruthy();
    expect(screen.queryByText('No admitted pull requests')).toBeNull();
  });

  it('activates overlapping_edit with proximity evidence for a head-matched encounter', async () => {
    const indexedHead = INBOX.pull_requests[0]!.indexed_head_commit_id;
    const scope = {
      project_id: 'project.alpha',
      repository_id: 'repository.alpha',
      worktree_id: 'worktree.alpha',
      branch_ref: 'refs/heads/feature/delivery',
      head_commit_id: indexedHead,
    };
    const proximity = {
      state: 'complete',
      page: {
        scope,
        source_generation: 'generation.proximity.1',
        observed_at: 1_700_000_200_000_000,
        expires_at: 1_700_000_500_000_000,
        encounters: [
          {
            encounter_id: 'sha256:overlap',
            scope,
            interval: { start: 1_700_000_000_000_000, end: 1_700_000_100_000_000 },
            participants: [
              {
                source: { provider: 'cursor', session_id: 'sess-a', source_key: null },
                agent_id: 'agent-a',
                worktree_id: 'worktree.alpha',
                worktree_root: '/tmp/alpha',
                branch_ref: 'refs/heads/feature/delivery',
                head_revision: indexedHead,
                access: 'write',
                activity: { start: 1_700_000_000_000_000, end: 1_700_000_050_000_000 },
                address: {
                  scope,
                  file: 'src/lib.rs',
                  span: { start_byte: 0, end_byte: 10 },
                  symbol: 'symbol',
                },
              },
            ],
            relation: { relation_kind: 'overlapping_edit', warning_class: 'same_file' },
            observed_at: 1_700_000_100_000_000,
            expires_at: 1_700_000_400_000_000,
            coverage: 'complete',
          },
        ],
      },
    } satisfies FeedbackProximityReadResultV1;

    renderDelivery(INBOX, 'ready', '/delivery', applicationEnvelope(proximity));

    const detail = await screen.findByRole('region', { name: 'Pull request detail' });
    expect(await within(detail).findByText('sha256:overlap:overlapping_edit')).toBeTruthy();
    expect(within(detail).queryByText('This source has no mounted Delivery authority.')).toBeNull();
  });
});
