import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { INBOX, OVERVIEW_ALPHA, OVERVIEW_LOCAL_ONLY } from '../../test/deliveryFixtures.ts';
import { PR_42, PR_43, renderDelivery } from '../../test/renderDelivery.tsx';
import { LANE_FIELD_LABEL } from './LaneField.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Delivery inbox · lanes field', () => {
  it('zooms semantically through the URL scope and prints exact lane counts', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX);
    const zoom = await screen.findByRole('group', { name: 'Semantic zoom' });
    expect(within(zoom).getByRole('button', { name: 'Portfolio' }).getAttribute('aria-pressed')).toBe('true');
    expect(within(zoom).getByText('2 lanes · 3 bars drawn · 0 compressed · 2 threads')).toBeTruthy();

    const field = screen.getByRole('group', { name: LANE_FIELD_LABEL });
    expect(within(field).queryByText('provider reads · observed')).toBeNull();
    await user.click(within(field).getAllByRole('button')[0]!);
    expect(screen.getByTestId('location').textContent).toBe(`?pr=${PR_42}`);
    expect(within(zoom).getByText('2 lanes · 2 bars drawn · 1 compressed · 0 threads')).toBeTruthy();
    expect(within(field).getByText('provider reads · observed')).toBeTruthy();

    await user.click(within(zoom).getByRole('button', { name: 'Portfolio' }));
    expect(screen.getByTestId('location').textContent).toBe('');
  });

  it('makes the full 44px row band the target and selects with Enter', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX);
    const field = await screen.findByRole('group', { name: LANE_FIELD_LABEL });
    const bars = within(field).getAllByRole('button');
    expect(bars.map((bar) => bar.getAttribute('data-delivery-mark'))).toEqual([
      'project.alpha:github:42',
      'project.alpha:github:43',
      'project.beta:github:8',
    ]);
    expect(bars.map((bar) => bar.querySelector('[data-hit-band]')?.getAttribute('height'))).toEqual(['44', '44', '44']);
    expect(bars[1]!.getAttribute('aria-label')).toBe(
      'Pull request #43 · Persist retry backoff · no observation time served · no active attention · current · joined by served evidence · provider head not observed · no read snapshot served',
    );
    bars[1]!.focus();
    await user.keyboard('{Enter}');
    expect(screen.getByTestId('location').textContent).toBe('?pr=project.alpha%3Agithub%3A43');
  });

  it('names each drawn amber source, counts the unevaluated ones, and prints thread grades', async () => {
    renderDelivery(INBOX);
    const field = await screen.findByRole('group', { name: LANE_FIELD_LABEL });
    expect(screen.getByRole('list', { name: 'Attention legend' }).textContent).toBe(
      'amber = active attention, named sourceCI = ci failureREVIEW = unresolved review1 attention source not evaluated',
    );
    expect([...field.querySelectorAll('[data-thread]')].map((thread) => [thread.getAttribute('data-grade'), thread.textContent])).toEqual([
      ['explicit', 'WORK · EXPLICIT'],
      ['inferred', 'AGENT · INFERRED'],
    ]);
  });
});

describe('Delivery umbrella · lanes field', () => {
  it('draws only the umbrella members and its own basis thread, with no umbrella node', async () => {
    renderDelivery(INBOX, { route: '/delivery?mode=umbrella' });
    const field = await screen.findByRole('group', { name: LANE_FIELD_LABEL });
    expect(within(field).getAllByRole('button').map((bar) => bar.getAttribute('data-delivery-mark'))).toEqual([
      'project.alpha:github:42',
      'project.beta:github:8',
    ]);
    expect([...field.querySelectorAll('[data-thread]')].map((thread) => thread.textContent)).toEqual(['WORK · EXPLICIT']);
  });
});

describe('Delivery journey · transit', () => {
  it('draws CI / review with no served item as a NO EVIDENCE band naming the missing authorities', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_43}`, overview: OVERVIEW_LOCAL_ONLY });
    const transit = await screen.findByRole('list', { name: 'Delivery transit' });
    const verification = within(transit).getByRole('listitem', { name: 'CI / review · NO EVIDENCE · UNAVAILABLE' });
    expect(verification.getAttribute('data-station-state')).toBe('no_evidence');
    expect(within(verification).getByText('no evidence')).toBeTruthy();
    expect(within(verification).getByText('Reviews · not published · requires github_read_authority')).toBeTruthy();
    expect(within(transit).getByRole('listitem', { name: 'code to verification · UNAVAILABLE · no evidence to join' })).toBeTruthy();
  });

  it('prints unserved provider lanes beside the served CI / review attention', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_LOCAL_ONLY });
    const transit = await screen.findByRole('list', { name: 'Delivery transit' });
    expect([...transit.querySelectorAll('[data-station]')].map((node) => [node.getAttribute('data-station'), node.getAttribute('data-station-state')])).toEqual([
      ['session', 'evidence'],
      ['code', 'evidence'],
      ['verification', 'evidence'],
      ['next', 'evidence'],
    ]);
    const verification = within(transit).getByRole('listitem', { name: /^CI \/ review/ });
    expect(within(verification).getByText('Checks · not published · requires ci_provider_read_authority')).toBeTruthy();
    expect(within(transit).getByRole('listitem', { name: 'session to code · INFERRED · session–Git relation' })).toBeTruthy();
  });

  it('sets plate text on the 14px body token and keeps branches collapsed with exact counts', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { route: `/delivery?mode=journey&pr=${PR_42}`, overview: OVERVIEW_ALPHA });
    const transit = await screen.findByRole('list', { name: 'Delivery transit' });
    expect(within(transit).getByText('feat(ingest): add retry backoff').className).toContain('text-body');
    const branches = within(transit).getByRole('button', {
      name: '▸ branches · 1 objective · 1 session · 2 agents',
    });
    expect(branches.getAttribute('aria-expanded')).toBe('false');
    expect(within(transit).queryByText('session · session.alpha.1')).toBeNull();
    await user.click(branches);
    expect(within(transit).getByText('session · session.alpha.1')).toBeTruthy();
    expect(within(transit).getByText('agent · planner')).toBeTruthy();
    await user.click(within(transit).getByRole('button', { name: /feat\(ingest\): add retry backoff/ }));
    expect(screen.getByTestId('location').textContent).toContain('episode=commits%3A');
  });
});
