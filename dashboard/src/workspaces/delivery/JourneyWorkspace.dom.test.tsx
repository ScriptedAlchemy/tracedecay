import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { HEAD_ALPHA, INBOX, OVERVIEW_ALPHA, OVERVIEW_LOCAL_ONLY } from '../../test/deliveryFixtures.ts';
import { PR_42, PR_43, renderDelivery } from '../../test/renderDelivery.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

/** PR #42 over OVERVIEW_ALPHA: 1 objective + 1 session + 2 agent-usage rows
 * (undated), 2 commits, 1 PR identity + 1 provider read, 2 reviews, 2 checks;
 * releases not served. */
const EPISODES_42 = 12;
const DATED_42 = 8;

describe('JourneyWorkspace', () => {
  it('renders the field, the exact table with one row per episode, and the readouts', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_ALPHA });

    const transit = await screen.findByRole('list', { name: 'Delivery transit' });
    expect(within(transit).getAllByRole('button', { pressed: false })).toHaveLength(EPISODES_42 - 1);

    const episodes = screen.getByRole('region', { name: 'Journey episodes' });
    const table = within(episodes).getByRole('table');
    expect(within(table).getAllByRole('row')).toHaveLength(EPISODES_42 + 1);

    const readings = screen.getByLabelText('Journey readings');
    expect(within(readings).getByText('episodes').nextElementSibling?.textContent).toContain('12');
    expect(within(readings).getByText('dated').nextElementSibling?.textContent).toContain('8');
    expect(within(readings).getByText('undated').nextElementSibling?.textContent).toContain('4');
    expect(within(readings).getByText('lanes served').nextElementSibling?.textContent).toContain('7');
    expect(within(readings).getByText('8 total')).toBeTruthy();
    expect(within(readings).getByText(/05-09 15:00 → 05-09 21:00/)).toBeTruthy();

    expect(screen.getByRole('button', { name: 'Start review' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Back to inbox' })).toBeTruthy();
    expect(screen.getByRole('navigation', { name: 'Journey breadcrumb' }).textContent).toContain('#42');
  });

  it('names the pull request by its repository number, never the provider internal id', async () => {
    const row = INBOX.pull_requests[0]!;
    const pullRequest = {
      ...row.pull_request,
      pull_request_id: '4596824491',
      identity: { ...row.pull_request.identity!, number: 741 },
    };
    renderDelivery(
      { ...INBOX, pull_requests: [{ ...row, pull_request: pullRequest }, ...INBOX.pull_requests.slice(1)] },
      {
        route: `/delivery?mode=journey&pr=${PR_42}`,
        overview: {
          ...OVERVIEW_ALPHA,
          pull_requests: {
            state: 'ready',
            value: {
              expected_head_commit: HEAD_ALPHA,
              retained_head_commit: HEAD_ALPHA,
              total_retained: 1,
              truncated: false,
              items: [pullRequest],
            },
          },
        },
      },
    );
    await screen.findByRole('list', { name: 'Delivery transit' });
    const breadcrumb = screen.getByRole('navigation', { name: 'Journey breadcrumb' }).textContent;
    expect(breadcrumb).toContain('#741');
    expect(document.body.textContent).not.toContain('4596824491');
  });

  it('keeps undated membership records off the axis and prints their basis grade', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_ALPHA });
    const episodes = await screen.findByRole('region', { name: 'Journey episodes' });
    const table = within(episodes).getByRole('table');

    const objective = within(table).getByRole('button', { name: 'Work objective work.retry-backoff' }).closest('tr')!;
    expect(within(objective).getAllByText('undated').length).toBeGreaterThanOrEqual(1);
    expect(within(objective).getByText('WORK / EXPLICIT')).toBeTruthy();

    const session = within(table).getByRole('button', { name: 'Session session.alpha.1' }).closest('tr')!;
    expect(within(session).getByText('TRANSCRIPT / INFERRED')).toBeTruthy();
    expect(within(session).getByRole('link').getAttribute('href')).toBe('/loom?loomSession=session.alpha.1');

    const rows = within(table).getAllByRole('row').slice(1);
    const undatedIndex = rows.findIndex((row) => row.textContent?.startsWith('undated'));
    expect(undatedIndex).toBe(DATED_42);
    expect(within(table).getAllByText('OBSERVED').length).toBeGreaterThan(0);
    expect(within(table).getAllByText('EVENT').length).toBeGreaterThan(0);
  });

  it('lists every lane whose authority is not served with the daemon reason', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_ALPHA });
    const gaps = await screen.findByRole('region', { name: 'Journey gaps' });
    expect(within(gaps).getByText(/requires github_read_authority/)).toBeTruthy();
    expect(within(gaps).getByText('Releases')).toBeTruthy();
    // Agent usage is served on this branch, so the agents lane is not a gap.
    expect(within(gaps).queryByText('Agents')).toBeNull();
    expect(within(gaps).queryByText('No gap reported by the joined authorities.')).toBeNull();
  });

  it('names the correlation authority when no session span places agents on the branch', async () => {
    renderDelivery(INBOX, {
      route: `/delivery?mode=journey&pr=${PR_42}`,
      overview: {
        ...OVERVIEW_ALPHA,
        agent_usage: {
          state: 'not_published',
          reason: 'no session has recorded a Git branch span yet',
          required_authority: 'session-Git correlation index',
        },
      },
    });
    const gaps = await screen.findByRole('region', { name: 'Journey gaps' });
    expect(within(gaps).getByText('Agents')).toBeTruthy();
    expect(within(gaps).getByText(/requires session-Git correlation index/)).toBeTruthy();
    const ledger = screen.getByText('Agent usage').closest('li')!;
    expect(within(ledger).getByText('source · session usage')).toBeTruthy();
  });

  it('reads per-agent tokens and tool calls in the inspector', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_ALPHA });
    const episodes = await screen.findByRole('region', { name: 'Journey episodes' });
    await user.click(within(within(episodes).getByRole('table')).getByRole('button', { name: 'planner' }));
    const inspector = screen.getByRole('region', { name: 'Episode detail' });
    expect(within(inspector).getByText('21,500 tokens')).toBeTruthy();
    expect(within(inspector).getByText('41')).toBeTruthy();
    expect(within(inspector).getByText('2 · 2 with usage')).toBeTruthy();

  });

  it('selects an episode through the URL and reads it in the inspector with a Compare pivot', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_ALPHA });
    const episodes = await screen.findByRole('region', { name: 'Journey episodes' });
    const table = within(episodes).getByRole('table');

    const inspector = screen.getByRole('region', { name: 'Episode detail' });
    expect(within(inspector).getByText(/Select an episode/)).toBeTruthy();

    await user.click(within(table).getByRole('button', { name: 'src/ingest/retry.ts:142' }));
    expect(screen.getByTestId('location').textContent).toContain('episode=reviews%3Areview.R1');
    expect(within(table).getByRole('button', { name: 'src/ingest/retry.ts:142', pressed: true })).toBeTruthy();

    expect(within(inspector).getByText('changes requested · current · maintainer')).toBeTruthy();
    expect(within(inspector).getByText('REVIEW / EXACT')).toBeTruthy();
    expect(within(inspector).getByText(/OBSERVED/)).toBeTruthy();
    expect(within(inspector).getByText('Should shouldRetry accept attempt param?')).toBeTruthy();
    const compare = within(inspector).getByRole('link', { name: 'Open in Code · Compare' });
    expect(compare.getAttribute('href')).toContain('compare_file=src%2Fingest%2Fretry.ts');
    expect(within(inspector).getByRole('link', { name: 'Open in provider' }).getAttribute('href')).toBe(
      'https://github.com/example/alpha/pull/42#discussion_r1',
    );

    const transit = screen.getByRole('list', { name: 'Delivery transit' });
    expect(within(transit).getByRole('button', { name: /src\/ingest\/retry\.ts:142/, pressed: true })).toBeTruthy();
  });

  it('selects from a transit station and switches the review control once a thread is addressed', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, {
      route: `/delivery?mode=journey&pr=${PR_42}&thread=thread.1`,
      overview: OVERVIEW_ALPHA,
    });
    const transit = await screen.findByRole('list', { name: 'Delivery transit' });
    expect(screen.getByRole('button', { name: 'Continue review' })).toBeTruthy();

    await user.click(within(transit).getByRole('button', { name: /Integration tests/ }));
    expect(screen.getByTestId('location').textContent).toContain('episode=checks%3Acheck.integration');
    const inspector = screen.getByRole('region', { name: 'Episode detail' });
    expect(within(inspector).getByText('.github/workflows/ci.yml')).toBeTruthy();
    expect(within(inspector).getByText(/src\/ingest\/retry\.ts:140-146 · failure · retry exhausted/)).toBeTruthy();
    expect(within(inspector).getByText('cargo test')).toBeTruthy();
  });

  it('prints the typed gap when the selected PR is missing from the head-bound provider page', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_43}`, overview: OVERVIEW_ALPHA });
    const gaps = await screen.findByRole('region', { name: 'Journey gaps' });
    expect(within(gaps).getByText(/#43 is not among the 1 head-bound provider items/)).toBeTruthy();
    const transit = screen.getByRole('list', { name: 'Delivery transit' });
    expect(within(transit).getByText('+12 −2 · 2 files').closest('button')).toBeNull();
  });

  it('draws no provider episode and names the required authority when the provider is not published', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_LOCAL_ONLY });
    const transit = await screen.findByRole('list', { name: 'Delivery transit' });
    expect(screen.getAllByText(/requires github_read_authority/).length).toBeGreaterThanOrEqual(3);
    expect(screen.getAllByText(/requires ci_provider_read_authority/).length).toBeGreaterThanOrEqual(1);
    expect(within(transit).queryByRole('button', { name: /Unit tests|Integration tests|src\/ingest\/retry\.ts:142/ })).toBeNull();
    expect(within(transit).getByText('+120 −35 · 8 files').closest('button')).toBeNull();
    expect(within(transit).getAllByRole('button', { name: /add retry backoff|cover backoff jitter/ })).toHaveLength(2);
  });

  it('exposes no provider mutation control', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_ALPHA });
    await screen.findByRole('list', { name: 'Delivery transit' });
    expect(screen.queryByRole('button', { name: /merge|rerun|re-run|resolve|approve/i })).toBeNull();
    expect(screen.getAllByText(/read-only provider/i).length).toBeGreaterThan(0);
  });
});
