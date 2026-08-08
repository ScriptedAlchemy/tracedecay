/**
 * The CORTEX relief sheet, against a wire-true `StructureReadV1<Strata>`.
 *
 * jsdom has no 2D context and draws nothing, so what this suite protects is not
 * the picture but the three claims that make the picture admissible: every
 * region the field aggregates is carried in an accessible equivalent (including
 * the ones the drawing cap folds out), the sheet names the channels it is NOT
 * drawing rather than quietly omitting them, and a read that did not measure
 * renders as unmeasured instead of as empty terrain.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type {
  StrataClusterV1,
  StrataFileV1,
  StrataMeasurementV1,
} from '../../contracts/generated.ts';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { CortexRelief } from './CortexRelief.tsx';
import { MAX_DRAWN_REGIONS } from './cortexRelief.ts';

/** The canonical envelope from a shipped fixture, with this route's payload
 * swapped in — so the test exercises the real envelope acceptance path rather
 * than a hand-rolled wrapper that could drift from it. */
function envelope(payload: unknown) {
  const base = resolveFixture('/api/plugins/graph/overview') as Record<string, unknown>;
  return { ...base, payload };
}

function cluster(
  directory: string,
  overrides: Partial<StrataClusterV1> = {},
): StrataClusterV1 {
  const incoming = overrides.incoming_edges ?? 5;
  const outgoing = overrides.outgoing_edges ?? 7;
  return {
    directory,
    order: overrides.order ?? 0,
    file_count: overrides.file_count ?? 6,
    internal_edges: overrides.internal_edges ?? 12,
    incoming_edges: incoming,
    outgoing_edges: outgoing,
    boundary_edges: overrides.boundary_edges ?? incoming + outgoing,
  };
}

function file(path: string, depth: number): StrataFileV1 {
  return { path, depth, scc_size: 1, chain: [path] };
}

/** More regions than the drawing cap, so the folded set is never empty. */
function wideMeasurement(): StrataMeasurementV1 {
  const count = MAX_DRAWN_REGIONS + 4;
  const clusters = Array.from({ length: count }, (_, index) =>
    cluster(`src/mod${index}`, {
      order: index,
      file_count: 2 + (index % 5),
      internal_edges: index === 3 ? 0 : 4 + index,
    }),
  );
  return {
    algorithm: 'tarjan_scc_then_longest_path',
    cluster_ordering: 'dsm_boundary_edges_desc_then_file_count_desc',
    clusters,
    dependency_edge_kinds: ['calls', 'uses'],
    files: Array.from({ length: count }, (_, index) =>
      file(`src/mod${index}/a.rs`, index % 4),
    ),
    granularity: 'file',
    graph_generation: 'g-7',
    ideal_depth: 3,
    max_depth: 3,
    scan: {
      budget_ms: 4000,
      cache_scope: 'graph_generation',
      cache_state: 'hit',
      dependency_edges_examined: 1200,
      files_examined: 240,
      max_dependency_edges: 40_000,
      max_files: 20_000,
    },
  };
}

function serve(body: unknown, status = 200) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({ ok: status < 400, status, json: async () => body }) as Response),
  );
}

function renderCortex(focusPath: string | null = null) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <CortexRelief focusPath={focusPath} />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  // jsdom ships no 2D context and logs a "not implemented" notice on every
  // probe. Returning null explicitly is the same answer with none of the noise,
  // and it is the case the surface has to survive: the relief draws nothing and
  // the table carries the whole clustering.
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
  vi.stubGlobal('ResizeObserver', undefined);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('CortexRelief', () => {
  it('carries every aggregated region in an accessible equivalent, not just on the canvas', async () => {
    const measurement = wideMeasurement();
    serve(envelope({ status: 'measured', measurement }));
    const { container } = renderCortex();

    // One role="img" carrying the field's claims, and the canvas itself hidden
    // from assistive technology.
    const field = await screen.findByRole('img');
    const description = field.getAttribute('aria-label') ?? '';
    expect(description).toMatch(/Relief terrain of \d+ module regions/);
    expect(description).toContain('bedrock');
    expect(description).toContain('The table below carries the same regions as text');
    expect(container.querySelector('canvas')?.getAttribute('aria-hidden')).toBe('true');

    // The table is that equivalent: EVERY clustered region, including the ones
    // the drawing cap folds out, so a visual cap is never silent data loss.
    const table = screen.getByRole('table');
    const rows = within(table).getAllByRole('row').slice(1);
    expect(rows).toHaveLength(measurement.clusters.length);
    expect(rows.length).toBeGreaterThan(MAX_DRAWN_REGIONS);
    expect(within(table).getAllByText('folded')).toHaveLength(
      measurement.clusters.length - MAX_DRAWN_REGIONS,
    );
    expect(within(table).getAllByText('drawn')).toHaveLength(MAX_DRAWN_REGIONS);
    for (const clustered of measurement.clusters) {
      expect(within(table).getByText(clustered.directory)).toBeTruthy();
    }
  });

  it('states the aggregation it performed, in counted figures', async () => {
    serve(envelope({ status: 'measured', measurement: wideMeasurement() }));
    const { container } = renderCortex();
    await screen.findByRole('table');

    const plate = container.querySelector('[data-testid="cortex-readout"]')!.textContent ?? '';
    expect(plate).toContain('module regions');
    expect(plate).toContain('depth strata');
    expect(plate).toContain('contour interval');
    expect(plate).toContain('edges / file');
    expect(plate).toContain('folded out');
    expect(plate).toContain(`cap ${MAX_DRAWN_REGIONS} regions`);
  });

  it('names the channels the read does not back instead of drawing them', async () => {
    serve(envelope({ status: 'measured', measurement: wideMeasurement() }));
    const { container } = renderCortex();
    await screen.findByRole('table');

    const key = container.querySelector('[data-testid="cortex-key"]')!.textContent ?? '';
    // What it does draw, each traced to the measurement it came from.
    expect(key).toContain('file-level dependency depth');
    expect(key).toContain('area carries the count');
    expect(key).toContain('files and not symbols');
    expect(key).toContain('index contour');
    // And the harder half: what it is not drawing, and why.
    expect(key).toContain('not on this sheet');
    expect(key).toContain('no git-churn read is exposed to this dashboard');
    expect(key).toContain('not region-pair edge counts');
    expect(key).toContain('live activity is published for the project and carries no path');
  });

  it('draws a measured zero as no relief rather than as flat ground', async () => {
    serve(envelope({ status: 'measured', measurement: wideMeasurement() }));
    renderCortex();
    const table = await screen.findByRole('table');
    // `src/mod3` was given zero internal edges by the fixture.
    expect(within(table).getAllByText('no relief').length).toBe(1);
    const key = screen.getByTestId('cortex-key').textContent ?? '';
    expect(key).toContain('1 without relief');
    expect(key).toContain('It is not missing from the sheet, and it is not flat ground');
  });

  it('rings the region the workspace focus lives in, composing with the lens', async () => {
    serve(envelope({ status: 'measured', measurement: wideMeasurement() }));
    renderCortex('src/mod2/a.rs');
    await screen.findByRole('table');
    expect(
      screen.getByText(/the symbol this workspace is focused on lives in/i).textContent,
    ).toContain('src/mod2');
    expect(screen.getByText(/Move the lens to TRACE to flood it/i)).toBeTruthy();
  });

  it('says so when the focused directory is not a drawn region', async () => {
    serve(envelope({ status: 'measured', measurement: wideMeasurement() }));
    renderCortex('third_party/x/a.rs');
    await screen.findByRole('table');
    expect(screen.getByText(/that directory is not one of the drawn regions/i)).toBeTruthy();
  });

  it('reads a region out in full when its row is selected', async () => {
    serve(envelope({ status: 'measured', measurement: wideMeasurement() }));
    const user = userEvent.setup();
    renderCortex();
    const table = await screen.findByRole('table');

    await user.click(within(table).getByRole('button', { name: /src\/mod5/ }));
    await waitFor(() => {
      expect(screen.getByTestId('cortex-selected')).toBeTruthy();
    });
    const readout = screen.getByTestId('cortex-selected').textContent ?? '';
    expect(readout).toContain('src/mod5');
    expect(readout).toContain('stratum');
    expect(readout).toContain('e/file');
    expect(readout).toContain('contours');
    expect(readout).toContain('in /');
  });

  it('renders an unmeasured read as unmeasured, never as empty terrain', async () => {
    serve(
      envelope({
        status: 'unmeasured',
        reason: 'graph_authority_unavailable',
        detail: 'the retained project graph is unavailable',
      }),
    );
    renderCortex();

    expect(
      await screen.findByText(/the dependency strata could not be measured/i),
    ).toBeTruthy();
    expect(screen.getByText(/graph_authority_unavailable/)).toBeTruthy();
    // No table, no canvas, no zeros: nothing is drawn in the absence's place.
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.queryByRole('img')).toBeNull();
  });

  it('renders a transport failure as a failure rather than a flat repository', async () => {
    serve({}, 500);
    renderCortex();

    expect(
      await screen.findByText(/the dependency strata could not be measured/i),
    ).toBeTruthy();
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.queryByText(/0 regions/)).toBeNull();
  });
});
