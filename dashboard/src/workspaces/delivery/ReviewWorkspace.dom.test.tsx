import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { INBOX, OVERVIEW_ALPHA, OVERVIEW_LOCAL_ONLY } from '../../test/deliveryFixtures.ts';
import { PR_42, renderDelivery } from '../../test/renderDelivery.tsx';

const REVIEW_ROUTE = `/delivery?mode=review&pr=${PR_42}`;

afterEach(() => {
  vi.unstubAllGlobals();
});

function region(name: string) {
  return screen.getByRole('region', { name });
}

function laneButton(lifecycle: string) {
  const lanes = screen.getByRole('group', { name: 'Review lanes' });
  return within(lanes).getByRole('button', { name: new RegExp(`^${lifecycle}\\b`) });
}

function checkRow(label: string): HTMLElement {
  const table = screen.getByRole('table', { name: 'Check matrix' });
  const row = within(table).getByRole('button', { name: label }).closest('tr');
  if (row === null) throw new Error(`no matrix row for ${label}`);
  return row;
}

describe('ReviewWorkspace · exact review coverage', () => {
  it('prints lifecycle counts, groups threads by path, types every check and names the provider identity', async () => {
    renderDelivery(INBOX, { route: REVIEW_ROUTE, overview: OVERVIEW_ALPHA });

    const threads = await screen.findByRole('region', { name: 'Review threads' });
    expect(within(laneButton('current')).getByText('1')).toBeTruthy();
    expect(within(laneButton('outdated')).getByText('1')).toBeTruthy();
    expect(within(laneButton('resolved')).getByText('0')).toBeTruthy();
    expect(within(threads).getByRole('heading', { name: 'src/ingest/config.ts' })).toBeTruthy();
    expect(within(threads).getByRole('heading', { name: 'src/ingest/retry.ts' })).toBeTruthy();
    expect(within(threads).getAllByRole('button', { pressed: false })).toHaveLength(2);

    const matrix = screen.getByRole('table', { name: 'Check matrix' });
    expect(within(matrix).getAllByRole('row')).toHaveLength(3);
    expect(within(checkRow('Integration tests')).getByText('Error')).toBeTruthy();
    expect(within(checkRow('Unit tests')).getByText('Ready')).toBeTruthy();

    const identity = region('Identity (exact)');
    expect(within(identity).getByText('cccccccccccc')).toBeTruthy();
    expect(within(identity).getByText('dddddddddddd')).toBeTruthy();
    expect(within(identity).queryByText(/not served/)).toBeNull();
  });

  it('filters threads by the lane in the URL while keeping every lane count, and writes lane toggles back', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { route: `${REVIEW_ROUTE}&lane=current`, overview: OVERVIEW_ALPHA });

    const threads = await screen.findByRole('region', { name: 'Review threads' });
    expect(within(threads).getAllByRole('button', { pressed: false })).toHaveLength(1);
    expect(within(threads).getByRole('button', { name: /src\/ingest\/retry\.ts:142/ })).toBeTruthy();
    expect(within(threads).queryByRole('heading', { name: 'src/ingest/config.ts' })).toBeNull();
    expect(laneButton('current').getAttribute('aria-pressed')).toBe('true');
    expect(within(laneButton('outdated')).getByText('1')).toBeTruthy();

    await user.click(laneButton('outdated'));
    expect(screen.getByTestId('location').textContent).toContain('lane=outdated');
    expect(screen.getByTestId('location').textContent).not.toContain('lane=current');
  });

  it('selects a thread through the URL and shows its provider anchor, Compare pivot and read-only actions', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { route: REVIEW_ROUTE, overview: OVERVIEW_ALPHA });

    const threads = await screen.findByRole('region', { name: 'Review threads' });
    await user.click(within(threads).getByRole('button', { name: /src\/ingest\/retry\.ts:142/ }));
    expect(screen.getByTestId('location').textContent).toContain('thread=review.R1');

    const selected = region('Selected review thread');
    expect(within(selected).getByText('Should shouldRetry accept attempt param?')).toBeTruthy();
    const provider = within(selected).getByRole('link', { name: 'Open in provider' });
    expect(provider.getAttribute('href')).toBe('https://github.com/example/alpha/pull/42#discussion_r1');
    expect(provider.getAttribute('target')).toBe('_blank');
    expect(within(selected).getByRole('link', { name: 'Compare in Code' }).getAttribute('href')).toContain(
      'compare_file=src%2Fingest%2Fretry.ts',
    );
    expect(within(selected).getByText('observed data, not a disposition')).toBeTruthy();
    expect(within(selected).getByText(/read-only provider/)).toBeTruthy();
  });

  it('prints an unavailable provider control when the observation carries no source_url', async () => {
    renderDelivery(INBOX, { route: `${REVIEW_ROUTE}&thread=review.R2`, overview: OVERVIEW_ALPHA });

    const selected = await screen.findByRole('region', { name: 'Selected review thread' });
    expect(within(selected).getByText(/no source_url served/)).toBeTruthy();
    expect(within(selected).queryByRole('link', { name: 'Open in provider' })).toBeNull();
    expect(within(selected).getByText('body not served')).toBeTruthy();
    expect(within(selected).getByText('src/ingest/config.ts (original line 12)')).toBeTruthy();
  });

  it('shows a selected check with its run identity and annotations when no thread is selected', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { route: REVIEW_ROUTE, overview: OVERVIEW_ALPHA });

    await screen.findByRole('table', { name: 'Check matrix' });
    await user.click(within(checkRow('Integration tests')).getByRole('button', { name: 'Integration tests' }));
    expect(screen.getByTestId('location').textContent).toContain('check=check.integration');

    const selected = region('Selected review thread');
    expect(within(selected).getByText('cr.2')).toBeTruthy();
    expect(within(selected).getByText(/src\/ingest\/retry\.ts:140-146 · failure · retry exhausted/)).toBeTruthy();
    expect(within(selected).getByRole('link', { name: 'Compare' }).getAttribute('href')).toContain(
      'compare_file=src%2Fingest%2Fretry.ts',
    );
  });

  it('renders the exact diff as a typed unavailable panel with a Compare pivot, never a code pane', async () => {
    renderDelivery(INBOX, { route: REVIEW_ROUTE, overview: OVERVIEW_ALPHA });

    const diff = await screen.findByRole('region', { name: 'Exact diff' });
    expect(within(diff).getByText(/not diff hunks/)).toBeTruthy();
    expect(diff.querySelector('pre, code')).toBeNull();
    const pivot = within(diff).getByRole('link', { name: 'Open exact revision in Code · Compare' });
    const params = new URLSearchParams(pivot.getAttribute('href')!.slice('/code?'.length));
    expect(params.get('view')).toBe('compare');
    expect(params.get('head')).toBe('feature/delivery');
    expect(params.get('head_revision')).toBe('a'.repeat(40));
  });

  it('prints the daemon reason for CI failure localization instead of an empty list', async () => {
    renderDelivery(INBOX, { route: REVIEW_ROUTE, overview: OVERVIEW_ALPHA });

    const localization = await screen.findByRole('region', { name: 'CI failure localization' });
    expect(within(localization).getByText(/no CI localization owner is mounted/)).toBeTruthy();
    expect(localization.querySelector('ul, ol')).toBeNull();
  });

  it('exposes no provider mutation control anywhere in the workspace', async () => {
    renderDelivery(INBOX, { route: REVIEW_ROUTE, overview: OVERVIEW_ALPHA });

    await screen.findByRole('region', { name: 'Review threads' });
    expect(
      screen.queryByRole('button', { name: /\b(merge|rerun|re-run|resolve|approve|post)\b/i }),
    ).toBeNull();
    expect(screen.queryByRole('button', { name: /^(merge|rerun|re-run|resolve|approve|post)$/i })).toBeNull();
    expect(screen.getAllByText(/read-only provider/i).length).toBeGreaterThan(0);
  });

  it('keeps local Git evidence when the provider projections are not published', async () => {
    renderDelivery(INBOX, { route: REVIEW_ROUTE, overview: OVERVIEW_LOCAL_ONLY });

    const threads = await screen.findByRole('region', { name: 'Review threads' });
    expect(within(threads).getByText(/not_published/)).toBeTruthy();
    expect(within(threads).getByText(/requires github_read_authority/)).toBeTruthy();
    expect(within(threads).queryByRole('button')).toBeNull();

    const matrix = region('Check matrix');
    expect(within(matrix).getByText(/not_published/)).toBeTruthy();
    expect(within(matrix).getByText(/requires ci_provider_read_authority/)).toBeTruthy();
    expect(screen.queryByRole('table', { name: 'Check matrix' })).toBeNull();

    expect(within(region('Commits (2)')).getByText('feat(ingest): add retry backoff')).toBeTruthy();
    expect(within(laneButton('current')).getByText('—')).toBeTruthy();
    expect(within(region('Identity (exact)')).getAllByText('— not served').length).toBeGreaterThan(0);
  });

  it('renders a denied overview as the centered plate and discloses no thread or check', async () => {
    renderDelivery(INBOX, { route: REVIEW_ROUTE, overview: OVERVIEW_ALPHA, overviewStatus: 403 });

    expect(await screen.findByText('Denied')).toBeTruthy();
    expect(screen.getByRole('heading', { name: 'PR review' })).toBeTruthy();
    expect(screen.queryByText(/Should shouldRetry/)).toBeNull();
    expect(screen.queryByRole('table', { name: 'Check matrix' })).toBeNull();
    expect(screen.queryByRole('region', { name: 'Review threads' })).toBeNull();
  });
});
