import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { DeliveryInboxV1 } from '../../contracts/generated.ts';
import {
  INBOX,
  INBOX_BRANCH_ONLY,
  OVERVIEW_ALPHA,
  OVERVIEW_LOCAL_ONLY,
} from '../../test/deliveryFixtures.ts';
import { PR_42, renderDelivery } from '../../test/renderDelivery.tsx';
import { LANE_FIELD_LABEL } from './LaneField.tsx';

/** Inbox already joined by the server, overlapping_edit is Active with typed
 * proximity evidence (no client `/api/feedback/proximity` re-join). */
const INBOX_WITH_PROXIMITY_ATTENTION: DeliveryInboxV1 = {
  ...INBOX,
  pull_requests: [
    {
      ...INBOX.pull_requests[0]!,
      attention: [
        INBOX.pull_requests[0]!.attention[0]!,
        {
          id: 'project.alpha:42:overlapping_edit',
          project_id: 'project.alpha',
          pull_request_id: '42',
          source: 'overlapping_edit',
          state: 'active',
          evidence: [
            {
              kind: 'proximity_encounter',
              encounter_id: 'sha256:overlap',
              relation: 'overlapping_edit',
            },
          ],
          coverage: 'complete',
          observed_at_micros: 1_700_000_100_000_000,
        },
      ],
    },
    ...INBOX.pull_requests.slice(1),
  ],
};


afterEach(() => {
  vi.unstubAllGlobals();
});

describe('DeliveryPage · inbox', () => {
  it('renders only admitted pull requests, selects through the URL and explains membership', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX);

    const queue = await screen.findByRole('region', { name: 'Admitted pull requests' });
    expect(within(queue).getAllByRole('button', { pressed: false })).toHaveLength(3);
    await user.click(within(queue).getByRole('button', { name: /Admit delivery inbox/ }));

    expect(screen.getByTestId('location').textContent).toContain(`pr=${PR_42}`);
    const detail = screen.getByRole('region', { name: 'Pull request detail' });
    const correlation = within(detail).getByRole('region', { name: /Correlation · 3 edges/ });
    expect(within(correlation).getByText('Branch and pull request reference')).toBeTruthy();
    expect(within(correlation).getByText('Shared Work objective')).toBeTruthy();
    expect(within(correlation).getByText('Session and Git relation')).toBeTruthy();
    expect(within(detail).getAllByText(/refs\/heads\/feature\/delivery/).length).toBeGreaterThan(0);
    expect(screen.getByText(/1 unrelated provider pull request excluded/)).toBeTruthy();
    expect(screen.queryByText('Unrelated provider PR')).toBeNull();
  });

  it('prints basis and grade on every correlation edge and never upgrades one', async () => {
    renderDelivery(INBOX, { route: `/delivery?pr=${PR_42}` });
    const detail = await screen.findByRole('region', { name: 'Pull request detail' });
    const correlation = within(detail).getByRole('region', { name: /Correlation · 3 edges/ });
    expect(within(correlation).getByText('COMMIT / EXACT')).toBeTruthy();
    expect(within(correlation).getByText('WORK / EXPLICIT')).toBeTruthy();
    expect(within(correlation).getByText('TRANSCRIPT / INFERRED')).toBeTruthy();
    expect(within(correlation).getByRole('link', { name: /Open in Loom/ }).getAttribute('href')).toBe(
      '/loom?loomSession=session.alpha.1',
    );
  });

  it('restores project, pull request, attention source, and evidence from the URL', async () => {
    renderDelivery(INBOX, {
      route: `/delivery?project=project.alpha&pr=${PR_42}&attention=ci_failure&evidence=anchor.ci.42`,
    });

    const detail = await screen.findByRole('region', { name: 'Pull request detail' });
    expect(within(detail).getByText('CI failure')).toBeTruthy();
    expect(within(detail).getByRole('button', { name: 'anchor.ci.42', pressed: true })).toBeTruthy();
    expect(within(detail).getByText('Overlapping edit')).toBeTruthy();
    expect((screen.getByRole('combobox', { name: 'Attention source' }) as HTMLSelectElement).value).toBe(
      'ci_failure',
    );
  });

  it('links to the existing verified Code views and a head-prefilled Compare', async () => {
    renderDelivery(INBOX, { route: `/delivery?pr=${PR_42}` });

    const shared = await screen.findByRole('link', { name: 'Open Shared Code' });
    const compare = screen.getByRole('link', { name: 'Open Compare' });
    expect(shared.getAttribute('href')).toBe('/code?view=shared-code');
    expect(compare.getAttribute('href')).toBe('/code?view=compare');
    const head = screen.getByRole('link', { name: 'Compare head revision' }).getAttribute('href')!;
    const params = new URLSearchParams(head.slice('/code?'.length));
    expect(params.get('head')).toBe('feature/delivery');
    expect(params.get('head_revision')).toBe('a'.repeat(40));
    expect(params.get('base')).toBeNull();
  });

  it('exposes no provider mutation: only read-only badges and links', async () => {
    renderDelivery(INBOX, { route: `/delivery?pr=${PR_42}` });
    await screen.findByRole('region', { name: 'Pull request detail' });
    for (const verb of [/merge/i, /rerun/i, /re-run/i, /resolve/i, /post/i, /approve/i]) {
      expect(screen.queryByRole('button', { name: verb })).toBeNull();
    }
    expect(screen.getAllByText(/read-only provider/i).length).toBeGreaterThan(0);
  });

  it('filters by status, provider state and unresolved evidence from the URL', async () => {
    renderDelivery(INBOX, { route: '/delivery?status=draft' });
    const queue = await screen.findByRole('region', { name: 'Admitted pull requests' });
    expect(within(queue).getAllByRole('button')).toHaveLength(1);
    expect(within(queue).getByRole('button', { name: /Persist retry backoff/ })).toBeTruthy();
  });

  it('narrows to unresolved pull requests only', async () => {
    renderDelivery(INBOX, { route: '/delivery?unresolved=1' });
    const queue = await screen.findByRole('region', { name: 'Admitted pull requests' });
    expect(within(queue).getAllByRole('button')).toHaveLength(1);
    expect(within(queue).getByRole('button', { name: /Admit delivery inbox/ })).toBeTruthy();
  });

  it('narrows by provider state without erasing the other projects', async () => {
    renderDelivery(INBOX, { route: '/delivery?provider=stale' });
    const queue = await screen.findByRole('region', { name: 'Admitted pull requests' });
    expect(within(queue).getByRole('button', { name: /Emit retry events/ })).toBeTruthy();
    expect(within(queue).queryByRole('button', { name: /Admit delivery inbox/ })).toBeNull();
    expect(screen.getByRole('button', { name: /alpha/ , pressed: false })).toBeTruthy();
  });

  it('renders the exact table fallback with basis, grade, scope and destinations', async () => {
    renderDelivery(INBOX, { route: '/delivery?layout=table' });
    const table = await screen.findByRole('table', { name: 'Admitted pull requests table' });
    const rows = within(table).getAllByRole('row');
    expect(rows).toHaveLength(4);
    expect(within(table).getAllByText('WORK / EXPLICIT')).toHaveLength(2);
    expect(within(table).getAllByRole('button', { name: 'Journey' })).toHaveLength(3);
    expect(screen.queryByRole('group', { name: LANE_FIELD_LABEL })).toBeNull();
  });

  it('separates the scoped project queue from correlated cross-project pull requests', async () => {
    renderDelivery(INBOX, { route: '/delivery?project=project.alpha' });
    const queue = await screen.findByRole('region', { name: 'Admitted pull requests' });
    expect(within(queue).getAllByRole('button')).toHaveLength(2);
    const related = screen.getByRole('region', { name: 'Related pull requests' });
    expect(within(related).getByRole('button', { name: /Emit retry events/ })).toBeTruthy();
    expect(within(related).getByText('WORK / EXPLICIT')).toBeTruthy();
    expect(within(related).getByText(/1 qualified by a served basis/)).toBeTruthy();
  });

  it('reports correlation as unavailable when only branch references are served', async () => {
    renderDelivery(INBOX_BRANCH_ONLY, { route: `/delivery?project=project.alpha&pr=${PR_42}` });
    const related = await screen.findByRole('region', { name: 'Related pull requests' });
    expect(within(related).getByText(/correlation unavailable · no cross-PR edge served/)).toBeTruthy();
    const detail = screen.getByRole('region', { name: 'Pull request detail' });
    expect(within(detail).getByText(/served no cross-PR correlation edge/)).toBeTruthy();
    expect(screen.getByText('correlation unavailable')).toBeTruthy();
  });

  it('selects a lane bar through the same URL the queue writes, and draws no umbrella node', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX);
    const field = await screen.findByRole('group', { name: LANE_FIELD_LABEL });
    await user.click(within(field).getByRole('button', { name: /Pull request #8/ }));
    expect(screen.getByTestId('location').textContent).toBe('?pr=project.beta%3Agithub%3A8');
    expect(within(field).queryByRole('button', { name: /Umbrella/ })).toBeNull();
  });

  it('renders provider-not-configured as a typed absence, not a transport failure or zero', async () => {
    renderDelivery({
      ...INBOX,
      projects: INBOX.projects.map((project) => ({ ...project, provider_state: 'not_configured' })),
      pull_requests: [],
      membership_edges: [],
      excluded_pull_requests: 0,
    });

    expect(await screen.findByText('Provider not configured')).toBeTruthy();
    expect(screen.getByText('No admitted pull requests')).toBeTruthy();
    expect(screen.getByText(/No provider read authority is configured/)).toBeTruthy();
    expect(screen.getByRole('link', { name: /Open Settings · Provider authority/ }).getAttribute('href')).toBe(
      '/settings',
    );
    expect(screen.queryByText(/transport/i)).toBeNull();
  });

  it('renders every-project-omitted as partial, not as a complete zero inbox', async () => {
    renderDelivery(
      {
        registry_state: 'ready',
        projects: [],
        pull_requests: [],
        membership_edges: [],
        omitted_projects: 1,
        excluded_pull_requests: 0,
      },
      { domainState: 'partial' },
    );

    expect(await screen.findByText('Every registered project was omitted')).toBeTruthy();
    expect(screen.getByText(/carry no indexed head yet/)).toBeTruthy();
    expect(screen.queryByText('No admitted pull requests')).toBeNull();
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
      { domainState: 'unknown' },
    );

    expect(await screen.findByText('Project registry unavailable')).toBeTruthy();
    expect(screen.queryByText('No admitted pull requests')).toBeNull();
  });

  it('renders server-joined overlapping_edit proximity evidence from the inbox', async () => {
    renderDelivery(INBOX_WITH_PROXIMITY_ATTENTION, { route: `/delivery?pr=${PR_42}` });

    const detail = await screen.findByRole('region', { name: 'Pull request detail' });
    expect(await within(detail).findByText('sha256:overlap:overlapping_edit')).toBeTruthy();
    expect(within(detail).queryByText('This source has no mounted Delivery authority.')).toBeNull();
  });

  it('disables journey and review until a pull request is selected, then addresses them by URL', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { overview: OVERVIEW_ALPHA });
    const modes = await screen.findByRole('navigation', { name: 'Delivery modes' });
    const journey = () => within(modes).getByRole('button', { name: 'Journey' }) as HTMLButtonElement;
    const review = () => within(modes).getByRole('button', { name: 'Review' }) as HTMLButtonElement;
    expect(journey().disabled).toBe(true);
    expect(review().disabled).toBe(true);
    expect(journey().title).toMatch(/Select a pull request/);

    const queue = await screen.findByRole('region', { name: 'Admitted pull requests' });
    await user.click(within(queue).getByRole('button', { name: /Admit delivery inbox/ }));
    expect(journey().disabled).toBe(false);
    await user.click(within(modes).getByRole('button', { name: 'Journey' }));
    expect(screen.getByTestId('location').textContent).toContain('mode=journey');
    expect(screen.getByTestId('location').textContent).toContain(`pr=${PR_42}`);
  });

  it('tells a deep link to journey without a selection that it requires one', async () => {
    renderDelivery(INBOX, { route: '/delivery?mode=journey' });
    expect(await screen.findByText('Journey requires a pull request')).toBeTruthy();
  });
});

describe('DeliveryPage · local-first wing', () => {
  it('keeps local Git evidence and the daemon reason when the scoped provider cannot serve', async () => {
    renderDelivery(
      {
        ...INBOX,
        projects: [
          { ...INBOX.projects[0]!, provider_state: 'not_published' },
          INBOX.projects[1]!,
        ],
        pull_requests: INBOX.pull_requests.filter((row) => row.project_id !== 'project.alpha'),
      },
      { route: '/delivery?project=project.alpha', overview: OVERVIEW_LOCAL_ONLY },
    );
    const wing = await screen.findByRole('region', { name: /Local-first/i });
    expect(within(wing).getByRole('link', { name: /Open Settings · Provider authority/ })).toBeTruthy();
    expect(await within(wing).findByText(/feat\(ingest\): add retry backoff/)).toBeTruthy();
    expect(within(wing).getByText(/feature\/delivery/)).toBeTruthy();
    expect(within(wing).getAllByText(/requires github_read_authority/).length).toBeGreaterThan(0);
    expect(within(wing).getByText(/not_published · requires github_read_authority/)).toBeTruthy();
    expect(within(wing).queryByRole('button', { name: /merge|rerun|post|approve/i })).toBeNull();
  });
});
