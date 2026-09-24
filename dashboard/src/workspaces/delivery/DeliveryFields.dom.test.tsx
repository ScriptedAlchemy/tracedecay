import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { INBOX, INBOX_BRANCH_ONLY, OVERVIEW_ALPHA, OVERVIEW_LOCAL_ONLY } from '../../test/deliveryFixtures.ts';
import { PR_42, PR_43, renderDelivery } from '../../test/renderDelivery.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Delivery renderer A · envelopes', () => {
  it('draws every admitted PR as a focusable mark and names amber in a legend', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { route: '/delivery?renderer=envelopes' });
    const field = await screen.findByRole('group', { name: 'Delivery inbox field · repositories as envelopes' });
    const marks = within(field).getAllByRole('button');
    expect(marks.map((mark) => mark.getAttribute('data-delivery-mark'))).toEqual([
      'project.alpha:github:42',
      'project.alpha:github:43',
      'project.beta:github:8',
    ]);
    expect(marks[0]!.getAttribute('aria-label')).toBe(
      'Pull request #42 · Admit delivery inbox · 155 lines changed · active attention CI, REVIEW · current · provider head = indexed head aaaaaaa',
    );
    const legend = screen.getByRole('list', { name: 'Attention legend' });
    expect(legend.textContent).toBe('amber = active attention, named sourceCI = ci failureREVIEW = unresolved review1 attention source not evaluated');

    marks[1]!.focus();
    await user.keyboard('{Enter}');
    expect(screen.getByTestId('location').textContent).toBe('?renderer=envelopes&pr=project.alpha%3Agithub%3A43');
  });

  it('prints the typed absence on hollow marks when only branch references are served', async () => {
    renderDelivery(INBOX_BRANCH_ONLY, { route: '/delivery?renderer=envelopes' });
    const field = await screen.findByRole('group', { name: 'Delivery inbox field · repositories as envelopes' });
    expect(within(field).getAllByText('not joined')).toHaveLength(2);
    expect(within(field).getByText('not joined · head unobserved')).toBeTruthy();
    expect(field.querySelectorAll('[data-link]')).toHaveLength(0);
  });
});

describe('Delivery renderer B · transit', () => {
  it('reads the unselected inbox as a departure board of four stations per PR', async () => {
    renderDelivery(INBOX, { route: '/delivery?renderer=transit' });
    const board = await screen.findByRole('list', { name: 'Transit board' });
    const first = within(board).getAllByRole('button')[0]!;
    expect(within(first).getByLabelText('Agent session EVIDENCE')).toBeTruthy();
    expect(within(first).getByLabelText('Next action EVIDENCE').textContent).toBe('2 open');
    const second = within(board).getAllByRole('button')[1]!;
    expect(within(second).getByLabelText('CI / review NO EVIDENCE')).toBeTruthy();
  });

  it('prints why a scoped repository draws no departure row', async () => {
    renderDelivery(INBOX, { route: '/delivery?renderer=transit&project=project.beta&status=merged' });
    const undrawn = await screen.findByRole('list', { name: 'Repositories without a drawn row' });
    expect(within(undrawn).getAllByRole('listitem').map((item) => item.textContent)).toEqual([
      'alpha 2 admitted · none match the current filters',
      'beta 1 admitted · none match the current filters',
    ]);
  });

  it('draws CI / review with no served item as a NO EVIDENCE band naming the missing authorities', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&renderer=transit&pr=${PR_43}`, overview: OVERVIEW_LOCAL_ONLY });
    const transit = await screen.findByRole('list', { name: 'Delivery transit' });
    const verification = within(transit).getByRole('listitem', { name: 'CI / review · NO EVIDENCE · UNAVAILABLE' });
    expect(verification.getAttribute('data-station-state')).toBe('no_evidence');
    expect(within(verification).getByText('no evidence')).toBeTruthy();
    expect(within(verification).getByText('Reviews · not published · requires github_read_authority')).toBeTruthy();
    expect(within(transit).getByRole('listitem', { name: 'code to verification · UNAVAILABLE · no evidence to join' })).toBeTruthy();
  });

  it('prints unserved provider lanes beside the served CI / review attention', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&renderer=transit&pr=${PR_42}`, overview: OVERVIEW_LOCAL_ONLY });
    const transit = await screen.findByRole('list', { name: 'Delivery transit' });
    const stations = [...transit.querySelectorAll('[data-station]')].map((node) => [
      node.getAttribute('data-station'),
      node.getAttribute('data-station-state'),
    ]);
    expect(stations).toEqual([
      ['session', 'evidence'],
      ['code', 'evidence'],
      ['verification', 'evidence'],
      ['next', 'evidence'],
    ]);
    const verification = within(transit).getByRole('listitem', { name: /^CI \/ review/ });
    expect(within(verification).getByText('Checks · not published · requires ci_provider_read_authority')).toBeTruthy();
    expect(within(transit).getByRole('listitem', { name: 'session to code · INFERRED · session–Git relation' })).toBeTruthy();
  });

  it('selects a journey episode from a station and expands collapsed branches with exact counts', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { route: `/delivery?mode=journey&renderer=transit&pr=${PR_42}`, overview: OVERVIEW_ALPHA });
    const transit = await screen.findByRole('list', { name: 'Delivery transit' });
    const branches = within(transit).getByRole('button', { name: '▸ branches · 1 objective · 1 session' });
    await user.click(branches);
    expect(within(transit).getByText('session · session.alpha.1')).toBeTruthy();
    await user.click(within(transit).getByRole('button', { name: /feat\(ingest\): add retry backoff/ }));
    expect(screen.getByTestId('location').textContent).toContain('episode=commits%3A');
  });
});

describe('Delivery renderer C · lanes', () => {
  it('zooms semantically through the URL scope and prints exact lane counts', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { route: '/delivery?renderer=lanes' });
    const zoom = await screen.findByRole('group', { name: 'Semantic zoom' });
    expect(within(zoom).getByRole('button', { name: 'Portfolio' }).getAttribute('aria-pressed')).toBe('true');
    expect(within(zoom).getByText('2 lanes · 3 bars drawn · 0 compressed · 2 threads')).toBeTruthy();

    const field = screen.getByRole('group', { name: 'Dense delivery field · repositories by observed time' });
    await user.click(within(field).getAllByRole('button')[0]!);
    expect(screen.getByTestId('location').textContent).toBe(`?renderer=lanes&pr=${PR_42}`);
    expect(within(zoom).getByText('2 lanes · 2 bars drawn · 1 compressed · 0 threads')).toBeTruthy();
    expect(within(field).getByText('provider reads · observed')).toBeTruthy();

    await user.click(within(zoom).getByRole('button', { name: 'Portfolio' }));
    expect(screen.getByTestId('location').textContent).toBe('?renderer=lanes');
  });
});
