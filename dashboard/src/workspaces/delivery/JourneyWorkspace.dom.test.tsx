import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { INBOX, OVERVIEW_ALPHA, OVERVIEW_LOCAL_ONLY } from '../../test/deliveryFixtures.ts';
import { PR_42, PR_43, renderDelivery } from '../../test/renderDelivery.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

/** PR #42 over OVERVIEW_ALPHA: 1 objective + 1 session (undated), 2 commits,
 * 1 PR identity + 1 provider read, 2 reviews, 2 checks; releases not served. */
const EPISODES_42 = 10;

describe('JourneyWorkspace', () => {
  it('renders the field, the exact table with one row per episode, and the readouts', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_ALPHA });

    const field = await screen.findByRole('group', { name: 'PR journey field' });
    expect(within(field).getAllByRole('button')).toHaveLength(EPISODES_42);

    const episodes = screen.getByRole('region', { name: 'Journey episodes' });
    const table = within(episodes).getByRole('table');
    expect(within(table).getAllByRole('row')).toHaveLength(EPISODES_42 + 1);

    const readings = screen.getByLabelText('Journey readings');
    expect(within(readings).getByText('episodes').nextElementSibling?.textContent).toContain('10');
    expect(within(readings).getByText('dated').nextElementSibling?.textContent).toContain('8');
    expect(within(readings).getByText('undated').nextElementSibling?.textContent).toContain('2');
    expect(within(readings).getByText('lanes served').nextElementSibling?.textContent).toContain('6');
    expect(within(readings).getByText('8 total')).toBeTruthy();
    expect(within(readings).getByText(/05-09 15:00 → 05-09 21:00/)).toBeTruthy();

    expect(screen.getByRole('button', { name: 'Start review' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Back to inbox' })).toBeTruthy();
    expect(screen.getByRole('navigation', { name: 'Journey breadcrumb' }).textContent).toContain('#42');
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
    expect(undatedIndex).toBe(EPISODES_42 - 2);
    expect(within(table).getAllByText('OBSERVED').length).toBeGreaterThan(0);
    expect(within(table).getAllByText('EVENT').length).toBeGreaterThan(0);
  });

  it('lists every lane whose authority is not served with the daemon reason', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_ALPHA });
    const gaps = await screen.findByRole('region', { name: 'Journey gaps' });
    expect(within(gaps).getByText(/requires github_read_authority/)).toBeTruthy();
    expect(within(gaps).getByText(/No agent attribution/)).toBeTruthy();
    expect(within(gaps).getByText('Releases')).toBeTruthy();
    expect(within(gaps).getByText('Agents')).toBeTruthy();
    expect(within(gaps).queryByText('No gap reported by the joined authorities.')).toBeNull();

    const lanes = screen.getByRole('list', { name: 'Journey lanes' });
    expect(within(lanes).getAllByText('unavailable')).toHaveLength(2);
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

    const field = screen.getByRole('group', { name: 'PR journey field' });
    expect(within(field).getByRole('button', { name: 'Reviews · src/ingest/retry.ts:142 · EXACT', pressed: true })).toBeTruthy();
  });

  it('selects from the field node and switches the review control once a thread is addressed', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, {
      route: `/delivery?mode=journey&pr=${PR_42}&thread=thread.1`,
      overview: OVERVIEW_ALPHA,
    });
    const field = await screen.findByRole('group', { name: 'PR journey field' });
    expect(screen.getByRole('button', { name: 'Continue review' })).toBeTruthy();

    await user.click(within(field).getByRole('button', { name: /Checks · Integration tests/ }));
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
    const field = screen.getByRole('group', { name: 'PR journey field' });
    expect(within(field).queryByRole('button', { name: /^Pull request ·/ })).toBeNull();
  });

  it('draws no provider node and names the required authority when the provider is not published', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_LOCAL_ONLY });
    const field = await screen.findByRole('group', { name: 'PR journey field' });
    expect(screen.getAllByText(/requires github_read_authority/).length).toBeGreaterThanOrEqual(3);
    expect(screen.getAllByText(/requires ci_provider_read_authority/).length).toBeGreaterThanOrEqual(1);
    expect(within(field).queryByRole('button', { name: /^Reviews ·/ })).toBeNull();
    expect(within(field).queryByRole('button', { name: /^Checks ·/ })).toBeNull();
    expect(within(field).queryByRole('button', { name: /^Pull request ·/ })).toBeNull();
    expect(within(field).getAllByRole('button', { name: /^Commits ·/ })).toHaveLength(2);
    const lanes = screen.getByRole('list', { name: 'Journey lanes' });
    expect(within(lanes).getAllByText('unavailable')).toHaveLength(5);
  });

  it('exposes no provider mutation control', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_ALPHA });
    await screen.findByRole('group', { name: 'PR journey field' });
    expect(screen.queryByRole('button', { name: /merge|rerun|re-run|resolve|approve/i })).toBeNull();
    expect(screen.getAllByText(/read-only provider/i).length).toBeGreaterThan(0);
  });
});
