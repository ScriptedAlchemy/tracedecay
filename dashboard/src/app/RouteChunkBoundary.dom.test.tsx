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
});
