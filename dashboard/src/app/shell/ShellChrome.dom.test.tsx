/**
 * The persistent shell's structural contract with
 * `mockups/ui-concept-v2/NAVIGATION.md`: one flat rail of fourteen numbered
 * channels in canonical order with a brand block that is identity only; a
 * register that names the scope, its canonical ID and the active channel; a
 * status strip whose registry cell reports the registry's own typed state.
 *
 * jsdom has no layout, so the 192px/48px/52px/32px geometry is asserted where
 * it is authored, the class names read the shell tokens, and measured by
 * the Playwright audits, not here.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen } from '@testing-library/react';
import type { ReactNode } from 'react';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  DashboardEnvelopeV1Schema,
  ProjectsPayloadV1Schema,
  type ProjectsPayloadV1,
} from '../../contracts/generated.ts';
import type { ProjectRegistryResult } from '../../data/query/projectRegistry.ts';
import { useScope } from '../../data/scope/store.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { CHANNELS } from '../channels.ts';
import { NavRail } from './NavRail.tsx';
import { ScopeBar } from './ScopeBar.tsx';
import { registryAuthorityReading } from './StatusStrip.tsx';

vi.mock('../../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: 'connecting' as const, lastEventAt: null }),
  useEventsConnection: () => null,
  useProjectionSync: () => ({ kind: 'unmounted' }) as const,
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
});

function queryWrapper(children: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: 0 } },
  });
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}

function renderRail(route = '/brain') {
  vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
  return render(
    queryWrapper(
      <MemoryRouter initialEntries={[route]}>
        <NavRail />
      </MemoryRouter>,
    ),
  );
}

describe('the navigation rail', () => {
  it('draws all fourteen channels once, in canonical order, with no grouping', () => {
    renderRail();
    const nav = screen.getByRole('navigation', { name: 'Workspaces' });
    const links = Array.from(nav.querySelectorAll('a'));
    expect(links.map((link) => link.getAttribute('aria-label'))).toEqual(
      CHANNELS.map((channel) => channel.label),
    );
    // Settings is channel 12 and sits at 12; it is not pinned to the foot.
    expect(links[11]?.getAttribute('aria-label')).toBe('Settings');
    expect(links.at(-1)?.getAttribute('aria-label')).toBe('Workflows');
    // No register dividers: the rail is one list of channels.
    expect(nav.querySelectorAll('section')).toHaveLength(0);
  });

  it('numbers every channel from the shared authority', () => {
    renderRail();
    const nav = screen.getByRole('navigation', { name: 'Workspaces' });
    const numbers = Array.from(nav.querySelectorAll('a')).map(
      (link) => link.querySelector('[aria-hidden]:nth-child(2)')?.textContent,
    );
    expect(numbers).toEqual(
      Array.from({ length: 14 }, (_, index) => String(index + 1).padStart(2, '0')),
    );
  });

  it('marks exactly the current route as selected', () => {
    renderRail('/code');
    const current = screen.getAllByRole('link').filter(
      (link) => link.getAttribute('aria-current') === 'page',
    );
    expect(current.map((link) => link.getAttribute('aria-label'))).toEqual(['Code']);
  });

  it('presents the brand block as identity only, not a link, not a status', () => {
    renderRail();
    const nav = screen.getByRole('navigation', { name: 'Workspaces' });
    const brand = nav.querySelector('[data-brand]');
    expect(brand).not.toBeNull();
    expect(brand?.textContent).toBe('TraceDecay');
    expect(brand?.querySelector('a, button')).toBeNull();
    expect(brand?.querySelector('[role="status"]')).toBeNull();
    // The glyph is decorative and says so.
    expect(brand?.querySelector('.td-trace-tail')?.getAttribute('aria-hidden')).toBe('true');
  });

  it('sizes the rail from the shell tokens, expanded and compact', () => {
    renderRail();
    const nav = screen.getByRole('navigation', { name: 'Workspaces' });
    expect(nav.className).toContain('w-[var(--shell-rail)]');
    expect(nav.className).toContain('max-md:w-[var(--shell-rail-compact)]');
  });
});

describe('the scope/workspace register', () => {
  it('shows Project: all and the active channel number and title', () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
    render(queryWrapper(<ScopeBar channel={{ path: 'code', label: 'Code' }} />));

    expect(screen.getByText('Project:')).toBeTruthy();
    expect(screen.getByText('all')).toBeTruthy();
    const channel = screen.getByLabelText('Active channel');
    expect(channel.getAttribute('data-active-channel')).toBe('code');
    expect(channel.textContent).toBe('06Code');
  });

  it('shows the reconciled label with its canonical ID as a separate field', () => {
    useScope.getState().selectProject('prj_7fc9a21e8b4', 'neuronet', 'active');
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
    render(queryWrapper(<ScopeBar channel={{ path: 'brain', label: 'Brain' }} />));

    expect(document.querySelector('[data-scope-label]')?.textContent).toBe('neuronet');
    expect(document.querySelector('[data-scope-id]')?.textContent).toBe('prj_7fc9a21e8b4');
    // The clear control is the only scope control the register offers.
    expect(
      screen.getByRole('button', { name: /^Clear project scope neuronet/ }),
    ).toBeTruthy();
  });

  it('draws no channel cell when the route names no channel', () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
    render(queryWrapper(<ScopeBar channel={null} />));
    expect(screen.queryByLabelText('Active channel')).toBeNull();
  });
});

describe('the registry authority reading', () => {
  function listing(over: Partial<ProjectsPayloadV1>): ProjectsPayloadV1 {
    return ProjectsPayloadV1Schema.parse({
      active_project_id: null,
      active_project_root: '/repos/none',
      error: null,
      limit: 100,
      project_tree: [],
      projects: [],
      status: 'ok',
      summary: null,
      truncated: false,
      ...over,
    });
  }
  /** Parsed through the generated envelope schema, as the fetcher parses it,
   * so the fixture cannot carry a shape the dashboard would have refused. */
  function envelope(payload: ProjectsPayloadV1): ProjectRegistryResult<ProjectsPayloadV1> {
    return {
      outcome: 'envelope',
      envelope: DashboardEnvelopeV1Schema(ProjectsPayloadV1Schema).parse(
        fixtureEnvelope(payload),
      ),
    };
  }

  it('is loading until the listing answers', () => {
    expect(registryAuthorityReading(undefined).value).toBe('loading');
  });

  it('is ready only for a complete ok listing', () => {
    expect(registryAuthorityReading(envelope(listing({}))).value).toBe('ready');
  });

  it('reports a page as truncated, not as the registry', () => {
    const reading = registryAuthorityReading(envelope(listing({ truncated: true, limit: 100 })));
    expect(reading.value).toBe('truncated');
    expect(reading.detail).toContain('100');
  });

  it('keeps a missing or unopenable registry as unavailable with its own reason', () => {
    const missing = registryAuthorityReading(
      envelope(listing({ status: 'missing_registry', error: 'no registry on this profile' })),
    );
    expect(missing.value).toBe('unavailable');
    expect(missing.detail).toBe('no registry on this profile');
    const unopenable = registryAuthorityReading(
      envelope(listing({ status: 'registry_unavailable', error: 'database locked' })),
    );
    expect(unopenable.value).toBe('unavailable');
    expect(unopenable.detail).toBe('database locked');
  });

  it('names each transport refusal rather than folding it into offline', () => {
    expect(registryAuthorityReading({ outcome: 'transport', state: 'offline' }).value).toBe(
      'offline',
    );
    expect(registryAuthorityReading({ outcome: 'transport', state: 'denied' }).value).toBe(
      'denied',
    );
    expect(registryAuthorityReading({ outcome: 'transport', state: 'unauthorized' }).value).toBe(
      'unauthorized',
    );
    expect(
      registryAuthorityReading({ outcome: 'transport', state: 'unsupported_schema' }).value,
    ).toBe('unsupported schema');
    expect(
      registryAuthorityReading({ outcome: 'transport', state: 'error', detail: 'HTTP 500' }),
    ).toMatchObject({ value: 'error', detail: 'HTTP 500' });
  });
});
