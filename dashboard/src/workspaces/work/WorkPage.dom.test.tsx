import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { WorkPage } from './WorkPage.tsx';

const CONTRACT_GATE_EXPLANATION =
  'No generated Work read model is available in this build. Kanban, DAG, timeline, causal, workload, runtime, and control state are withheld rather than inferred.';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('WorkPage contract gate', () => {
  it('names the workspace and unavailable contract boundary', () => {
    const { container } = render(<WorkPage />);

    expect(screen.getByRole('heading', { level: 1, name: 'Work' })).toBeTruthy();
    expect(screen.getByRole('region', { name: 'Work contract gate' })).toBeTruthy();
    expect(
      container.querySelector('[data-state="unsupported_schema"], [data-state="unknown"]'),
    ).not.toBeNull();
    expect(screen.queryByText('Ready')).toBeNull();
    expect(screen.queryByText('Complete · zero findings')).toBeNull();
  });

  it('explains exactly which state is withheld', () => {
    render(<WorkPage />);

    expect(screen.getByText(CONTRACT_GATE_EXPLANATION)).toBeTruthy();
  });

  it('renders no fabricated zeroes or unavailable commands', () => {
    const { container } = render(<WorkPage />);

    expect(container.textContent).not.toMatch(/\b0(?:\.0+)?\b/);
    expect(screen.queryAllByRole('button')).toHaveLength(0);
    expect(screen.queryAllByRole('link')).toHaveLength(0);
    expect(
      screen.queryByRole('button', { name: /create|admit|accept|cancel/i }),
    ).toBeNull();
  });

  it('does not fetch without a generated Work contract', () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);

    render(<WorkPage />);

    expect(fetchMock).not.toHaveBeenCalled();
  });
});
