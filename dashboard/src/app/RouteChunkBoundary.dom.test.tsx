// @vitest-environment jsdom

import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { RouteChunkBoundary } from './RouteChunkBoundary';

describe('RouteChunkBoundary', () => {
  it('renders the offline state when a lazy chunk fails, then remounts a fresh import on retry', async () => {
    let shouldFail = true;
    let loadCalls = 0;

    const load = () => {
      loadCalls += 1;
      if (shouldFail) {
        const error = new Error('Failed to fetch dynamically imported module');
        error.name = 'ChunkLoadError';
        return Promise.reject(error);
      }
      return Promise.resolve({
        default: () => <div>workspace ready</div>,
      });
    };

    const { container } = render(<RouteChunkBoundary load={load} />);

    expect(await screen.findByText('Offline')).toBeTruthy();
    expect(container.querySelector('[data-state="offline"]')).not.toBeNull();
    expect(
      screen.getByText(/the dashboard server is unreachable; this page's script chunk could not be loaded/),
    ).toBeTruthy();
    expect(screen.queryByText('workspace ready')).toBeNull();
    const callsAfterFailure = loadCalls;
    expect(callsAfterFailure).toBeGreaterThanOrEqual(1);

    shouldFail = false;
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));

    expect(await screen.findByText('workspace ready')).toBeTruthy();
    expect(screen.queryByText('Offline')).toBeNull();
    expect(loadCalls).toBeGreaterThan(callsAfterFailure);
  });

  /**
   * Every route mounts this same component type at the same Outlet slot, so a
   * client-side navigation is a PROP update, not a remount. The boundary must
   * swap to the new loader's page; pinning `lazy(load)` in the constructor
   * left the whole app stuck on whichever workspace rendered first.
   */
  it('swaps to the new page when the load prop changes without a remount', async () => {
    const loadBrain = () =>
      Promise.resolve({ default: () => <div>brain surface</div> });
    const loadCode = () =>
      Promise.resolve({ default: () => <div>code surface</div> });

    const { rerender } = render(<RouteChunkBoundary load={loadBrain} />);
    expect(await screen.findByText('brain surface')).toBeTruthy();

    rerender(<RouteChunkBoundary load={loadCode} />);
    expect(await screen.findByText('code surface')).toBeTruthy();
    expect(screen.queryByText('brain surface')).toBeNull();
  });

  it('clears a previous route failure when navigating to a different loader', async () => {
    const error = new Error('Failed to fetch dynamically imported module');
    error.name = 'ChunkLoadError';
    const loadBroken = () => Promise.reject(error);
    const loadHealthy = () =>
      Promise.resolve({ default: () => <div>healthy surface</div> });

    const { rerender } = render(<RouteChunkBoundary load={loadBroken} />);
    expect(await screen.findByText('Offline')).toBeTruthy();

    rerender(<RouteChunkBoundary load={loadHealthy} />);
    expect(await screen.findByText('healthy surface')).toBeTruthy();
    expect(screen.queryByText('Offline')).toBeNull();
  });
});
