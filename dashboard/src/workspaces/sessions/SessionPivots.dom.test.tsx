import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, useLocation } from 'react-router';
import { beforeEach, describe, expect, it } from 'vitest';

import { useScope } from '../../data/scope/store.ts';
import { SessionPivots, type SessionWorkspace } from './SessionPivots.tsx';
import type { SessionSelection } from './model.ts';

function CurrentLocation() {
  const location = useLocation();
  return <output data-testid="session-pivot-location">{location.pathname}{location.search}</output>;
}

function renderPivots(session: SessionSelection, from: SessionWorkspace = 'sessions') {
  return render(
    <MemoryRouter initialEntries={['/sessions']}>
      <SessionPivots session={session} from={from} />
      <CurrentLocation />
    </MemoryRouter>,
  );
}

beforeEach(() => {
  useScope.getState().selectAllProjects();
});

describe('session cross-workspace navigation', () => {
  it('opens Loom by a bare session ID when the recorded provider is unknown', async () => {
    const user = userEvent.setup();
    renderPivots({ provider: null, sessionId: 'session.alpha.1' });

    const loom = screen.getByRole('link', { name: 'Open in Loom' });
    expect(loom.getAttribute('href')).toBe('/loom?loomSession=session.alpha.1');
    await user.click(loom);
    expect(screen.getByTestId('session-pivot-location').textContent).toBe(
      '/loom?loomSession=session.alpha.1',
    );
  });

  it('keeps an admitted project scope and a provider-qualified Loom identity', () => {
    useScope.getState().selectProject('project-1', 'Alpha', 'active');
    renderPivots({ provider: 'codex', sessionId: 'shared' });

    const link = screen.getByRole('link', { name: 'Open in Loom' });
    const href = link.getAttribute('href');
    expect(href).not.toBeNull();
    const destination = new URL(href!, 'http://dashboard.test');
    expect(destination.pathname).toBe('/loom');
    expect(destination.searchParams.get('scope')).toBe('project-1');
    expect(destination.searchParams.get('scopeLabel')).toBe('Alpha');
    expect(destination.searchParams.get('loomSession')).toBe('["codex","shared"]');
  });

  it('does not offer a self-link or guess a provider for the other destinations', () => {
    renderPivots({ provider: null, sessionId: 'shared' }, 'loom');

    expect(screen.queryByRole('link', { name: 'Open in Loom' })).toBeNull();
    const sessions = screen.getByRole('link', { name: 'Open in Sessions' });
    const destination = new URL(sessions.getAttribute('href')!, 'http://dashboard.test');
    expect(destination.searchParams.get('sessionId')).toBe('shared');
    expect(destination.searchParams.has('sessionProvider')).toBe(false);
  });
});
