import { MemoryRouter, useLocation } from 'react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { LoomPage } from './LoomPage.tsx';
import { useScope } from '../../data/scope/store.ts';

/**
 * What this suite guards is not layout, it is the surface's claims.
 *
 * The Loom draws a real quantity (session start times) beside several absent
 * ones (durations, commits, edits, PRs). Every one of those absences is stated
 * in words somewhere on the page, and every one of those sentences is a thing a
 * future refactor could quietly delete while leaving a page that still renders
 * and still looks correct. These tests exist so that deletion fails.
 */

const NOW = Math.floor(Date.now() / 1000);

const SESSIONS = {
  available: true,
  total: 6053,
  sessions: [
    {
      session_id: 'sess-open',
      provider: 'cursor',
      title: 'Deliver Git primitive runtime',
      started_at: NOW - 7200,
      last_message_at: null,
      messages: 405,
      is_subagent: false,
      models: [{ model: 'gpt-5.6-sol-high' }],
    },
    {
      session_id: 'sess-closed',
      provider: 'claude',
      title: 'Verify QUERY scheduler',
      started_at: NOW - 10_800,
      last_message_at: NOW - 9000,
      messages: 50,
      is_subagent: false,
      models: [],
    },
    {
      session_id: 'sess-hollow',
      provider: 'codex',
      title: null,
      started_at: NOW - 3600,
      last_message_at: null,
      messages: 0,
      is_subagent: true,
      models: [],
    },
  ],
};

/** Canonically hydrated chain message with truthful nullable token accounting. */
function chainMessage(over: Record<string, unknown>) {
  return {
    session_id: 'sess-open',
    role: null,
    content: null,
    snippet: null,
    ordinal: null,
    timestamp: null,
    tool_name: null,
    token_count: null,
    token_count_provenance: null,
    pinned: 0,
    source: 'cursor',
    storage_kind: 'message',
    store_id: null,
    summary_node_ids: [],
    metadata_json: null,
    ...over,
  };
}

const CHAIN = {
  exists: true,
  session_id: 'sess-open',
  path: '/home/zack/.tracedecay/projects/project-loom/sessions.db',
  storage_scope: 'profile_sharded',
  limit: 200,
  next_cursor: null,
  has_more: false,
  has_more_messages: false,
  has_more_summary_nodes: false,
  counts: {
    message_count: 405,
    source_token_count: 0,
    summary_node_count: 0,
    summary_token_count: 0,
  },
  summary_nodes: [],
  messages: [
    chainMessage({
      message_id: 'm0',
      role: 'user',
      content: 'Verify durable code-generation restart.',
      ordinal: 0,
      token_count: 12,
      token_count_provenance: 'o200k_approximate',
    }),
    chainMessage({
      message_id: 'm1',
      role: 'assistant',
      content: 'Reading the reconciliation path.',
      ordinal: 1,
      tool_name: 'Read',
      token_count: 20,
      token_count_provenance: 'o200k_approximate',
    }),
    chainMessage({
      message_id: 'm2',
      role: 'assistant',
      content: 'Running the suite.',
      ordinal: 2,
      tool_name: 'Bash',
      token_count: null,
      token_count_provenance: null,
    }),
  ],
};

const TIMELINE = {
  path: 'daemon://session-temporal',
  storage_scope: 'project',
  exists: true,
  bucket: 'day',
  session_id: null,
  buckets: [
    {
      bucket: '2026-07-23',
      count: 1204,
      token_count: 41_000,
      token_count_provenance: 'o200k_approximate',
      known_message_count: 1204,
      unknown_message_count: 0,
    },
    {
      bucket: '2026-07-24',
      count: 8801,
      token_count: null,
      token_count_provenance: 'unavailable',
      known_message_count: 8700,
      unknown_message_count: 101,
    },
  ],
  node_buckets: [],
  undated: {
    count: 0,
    token_count: null,
    token_count_provenance: 'unavailable',
    known_message_count: 0,
    unknown_message_count: 0,
  },
  coverage: {
    limit: 400,
    returned_buckets: 2,
    total_dated_buckets: 2,
    truncated: false,
    ordering: 'most_recent',
    next_before_bucket: null,
  },
};

const TEMPORAL = {
  schema_revision: 1,
  scope: {
    project_id: 'project-loom',
    storage_mode: 'profile_sharded',
    store_root: '/profile/project-loom',
  },
  version: { entity_version: null, graph_version: null },
  time: { valid_time_micros: null, observation_time_micros: 1_784_700_000_000_000 },
  source_watermark: null,
  authorization: { outcome: 'authorized' },
  coverage: {
    completeness: 'complete',
    eligible: 3,
    examined: 3,
    matched: 3,
    excluded: 0,
    omitted: 0,
    unknown: 0,
    denominator: 3,
    unit: 'sessions',
    omission_reasons: [],
  },
  freshness: {
    state: 'fresh',
    observed_at_micros: 1_784_700_000_000_000,
    watermark: null,
  },
  domain_state: 'partial',
  legal_actions: [],
  payload: {
    available: true,
    total: 3,
    sessions: SESSIONS.sessions.map((session, index) => ({
      ...session,
      ended_at: index === 0 ? session.started_at + 1800 : null,
      edited_files_recorded: index === 0,
    })),
    source_statuses: [
      {
        id: 'session_commit',
        label: 'Session ↔ commit',
        state: 'ready',
        authority: 'commit_sessions',
        granularity: 'commit attribution',
        providers: ['cursor'],
        item_count: 1,
        reason: null,
        required_authority: null,
        coverage: {
          completeness: 'complete',
          eligible: 1,
          examined: 1,
          matched: 1,
          omitted: 0,
          unit: 'stored relation rows',
          reason:
            'provider-qualified commit_sessions rows for the displayed session page',
        },
      },
      {
        id: 'session_file',
        label: 'Session → edited file',
        state: 'partial',
        authority: 'sessions.metadata_json $.edited_files[]',
        granularity: 'recorded file rollup',
        providers: ['cursor'],
        item_count: 1,
        reason: 'provider-native rollups only',
        required_authority: null,
        coverage: {
          completeness: 'partial',
          eligible: 3,
          examined: 1,
          matched: 1,
          omitted: 2,
          unit: 'displayed sessions',
          reason: 'only sessions carrying an edited_files rollup are examined',
        },
      },
      {
        id: 'branch_worktree',
        label: 'Branch & worktree spans',
        state: 'ready',
        authority: 'session_git_spans',
        granularity: 'coalesced activity span',
        providers: ['cursor'],
        item_count: 1,
        reason: null,
        required_authority: null,
        coverage: {
          completeness: 'complete',
          eligible: 1,
          examined: 1,
          matched: 1,
          omitted: 0,
          unit: 'stored relation rows',
          reason:
            'provider-qualified session_git_spans rows for the displayed session page',
        },
      },
    ],
    commits: [
      {
        provider: 'cursor',
        session_id: 'sess-open',
        commit_sha: 'abc123def456',
        committed_at: NOW - 5000,
        branch: 'main',
        worktree: '/work/tracedecay',
        relation: 'produced',
        evidence: 'transcript',
        confidence: 100,
        span_overlap_kind: 'within_span',
      },
    ],
    edited_files: [
      {
        provider: 'cursor',
        session_id: 'sess-open',
        path: 'src/runtime.rs',
        change_type: 'edit',
        hunks: 2,
      },
    ],
    branch_spans: [
      {
        provider: 'cursor',
        session_id: 'sess-open',
        branch: 'main',
        worktree: '/work/tracedecay',
        first_at: NOW - 7200,
        last_at: NOW - 5400,
        event_count: 4,
        source: 'transcript',
      },
    ],
    temporal_refresh: {
      state: 'ready',
      active_generations: 1,
      latest_activated_at_micros: (NOW - 100) * 1_000_000,
      authority: 'session_temporal_generations maintained by the temporal refresh scheduler',
    },
  },
};

const TEMPORAL_RETRIEVAL_UNAVAILABLE = {
  ...TEMPORAL,
  coverage: {
    completeness: 'unknown',
    eligible: null,
    examined: null,
    matched: null,
    excluded: null,
    omitted: null,
    unknown: null,
    denominator: null,
    unit: 'records',
    omission_reasons: ['lcm_temporal_retrieval_not_mounted'],
  },
  freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
  domain_state: 'unknown',
  payload: null,
};

function readyEnvelope(payload: unknown) {
  return { ...TEMPORAL, domain_state: 'ready', payload };
}

function applicationEnvelope(payload: unknown) {
  return {
    kind: 'success',
    value: {
      binding_id: 'binding.dashboard.feedback_proximity.v1',
      contract: { schema_id: 'schema.application.feedback.proximity.result', schema_revision: 1 },
      request_id: 'request-proximity',
      scope: {
        project_id: 'project-loom',
        repository_id: 'repository-loom',
        worktree_id: 'worktree-loom',
        reference: 'refs/heads/main',
        scope_digest: 'sha256:scope',
      },
      outcome: { outcome: 'evidence', value: { payload } },
    },
  };
}

const PROXIMITY_SCOPE = {
  project_id: 'project-loom',
  repository_id: 'repository-loom',
  worktree_id: 'worktree-loom',
  branch_ref: 'refs/heads/main',
  head_commit_id: 'commit-main',
};

const PROXIMITY = {
  state: 'complete',
  page: {
    scope: PROXIMITY_SCOPE,
    source_generation: 'generation-proximity',
    observed_at: 1_784_700_000_000_000,
    expires_at: 1_784_700_030_000_000,
    encounters: [
      {
        encounter_id: 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
        scope: PROXIMITY_SCOPE,
        interval: {
          start: (NOW - 10_500) * 1_000_000,
          end: (NOW - 9_500) * 1_000_000,
        },
        participants: [
          {
            source: { provider: 'cursor', session_id: 'sess-open', source_key: null },
            agent_id: 'agent-open',
            worktree_id: 'worktree-loom',
            worktree_root: '/tmp/loom-open',
            branch_ref: 'refs/heads/main',
            head_revision: 'commit-open',
            access: 'write',
            activity: { start: (NOW - 10_800) * 1_000_000, end: (NOW - 9_000) * 1_000_000 },
            address: {
              scope: PROXIMITY_SCOPE,
              file: 'file-open',
              span: { start_byte: 4, end_byte: 12 },
              symbol: 'symbol-open',
            },
          },
          {
            source: { provider: 'claude', session_id: 'sess-closed', source_key: null },
            agent_id: 'agent-closed',
            worktree_id: 'worktree-loom',
            worktree_root: '/tmp/loom-closed',
            branch_ref: 'refs/heads/main',
            head_revision: 'commit-closed',
            access: 'write',
            activity: { start: (NOW - 10_800) * 1_000_000, end: (NOW - 9_000) * 1_000_000 },
            address: {
              scope: PROXIMITY_SCOPE,
              file: 'file-open',
              span: { start_byte: 4, end_byte: 12 },
              symbol: 'symbol-open',
            },
          },
        ],
        relation: { relation_kind: 'overlapping_edit', warning_class: 'same_file' },
        observed_at: 1_784_700_000_000_000,
        expires_at: 1_784_700_030_000_000,
        coverage: 'complete',
      },
    ],
  },
};

function serve(routes: Record<string, { status: number; body: unknown }>) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    // Project-scoped dashboard reads travel through the gateway prefix. The
    // fixtures name the canonical route after gateway resolution.
    const canonicalUrl = url.replace(/\/api\/projects\/[^/]+\//, '/api/');
    const hit = Object.entries(routes).find(([path]) => canonicalUrl.includes(path));
    const { status, body } = hit?.[1] ?? { status: 404, body: { status: 'not_found' } };
    return {
      ok: status >= 200 && status < 300,
      status,
      json: async () => body,
    } as Response;
  });
}

const HAPPY = {
  '/api/loom/temporal': { status: 200, body: TEMPORAL },
  '/api/feedback/proximity': { status: 200, body: applicationEnvelope(PROXIMITY) },
  '/api/plugins/hermes-lcm/session/': { status: 200, body: TEMPORAL_RETRIEVAL_UNAVAILABLE },
  '/api/plugins/hermes-lcm/timeline': { status: 200, body: TEMPORAL_RETRIEVAL_UNAVAILABLE },
};

function LocationProbe() {
  return <output data-testid="loom-url">{useLocation().search}</output>;
}

function renderLoom(routes: Record<string, { status: number; body: unknown }> = HAPPY, entry = '/loom?scope=project-loom') {
  vi.stubGlobal('fetch', serve(routes));
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  const result = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}><LoomPage /><LocationProbe /></MemoryRouter>
    </QueryClientProvider>,
  );
  return { ...result, client };
}

afterEach(() => {
  // Unmount before restoring the canvas stub: a query settling between the two
  // would otherwise repaint through jsdom's unimplemented method.
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  useScope.setState({ scope: { kind: 'all' } });
});

beforeEach(() => {
  // jsdom has no Canvas2D. Returning null explicitly takes the scene's typed
  // "layer unavailable" path without jsdom's "Not implemented" console error;
  // the SVG overlay these tests query is unaffected.
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
  // Loom's temporal authority is project-scoped. The dashboard no longer lets
  // the all-projects aggregate silently fall through to whichever project is
  // active, so this fixture selects the exact project represented by TEMPORAL.
  useScope.setState({
    scope: {
      kind: 'project',
      projectId: 'project-loom',
      label: 'project loom',
      activation: 'active',
    },
  });
});

const LANE = (provider: string, sessionId: string) => JSON.stringify([provider, sessionId]);

function hierarchyFor(nodes: Record<string, unknown>[], over: Record<string, unknown> = {}) {
  return {
    available: true, source: 'sessions', error: null, sessions_read: nodes.length,
    root_count: nodes.filter((node) => node['parent_session_id'] == null).length,
    edge_count: nodes.filter((node) => node['link'] === 'linked').length,
    max_depth: 1, missing_parent_count: 0, cycle_count: 0, truncated: false, nodes, ...over,
  };
}

function treeNode(sessionId: string, parent: string | null, provider = 'cursor', link = parent ? 'linked' : 'root') {
  return {
    session_id: sessionId, parent_session_id: parent, provider, link,
    parent_tool_use_id: parent ? 'tool-parent-7' : null, agent: `agent-${sessionId}`, title: null,
    started_at: NOW - 20_000, ended_at: null, depth: parent ? 1 : 0,
    descendants: parent ? 0 : 1, is_subagent: parent != null,
  };
}

/** The same three sessions, with the second one moved onto the first's provider so a
 * same-provider parent relation can be admitted. With `ordered`, the child also
 * starts after its parent; the fixture default keeps the child earlier. */
function linkedTemporal(ordered = false) {
  const temporal = structuredClone(TEMPORAL);
  temporal.payload.sessions[1]!.provider = 'cursor';
  if (ordered) {
    temporal.payload.sessions[1]!.started_at = NOW - 6600;
    temporal.payload.sessions[1]!.last_message_at = NOW - 6000;
  }
  return temporal;
}

describe('LoomPage', () => {
  it('restores encounter selection without clearing thread or replay URL state', async () => {
    const thread = encodeURIComponent(LANE('cursor', 'sess-open'));
    const encounter = PROXIMITY.page.encounters[0]!.encounter_id;
    renderLoom(
      { ...HAPPY, '/api/plugins/hermes-lcm/session/': { status: 200, body: readyEnvelope(CHAIN) } },
      `/loom?scope=project-loom&loomSession=${thread}&loomEvent=m0&loomEncounter=${encodeURIComponent(encounter)}`,
    );

    expect(await screen.findByText(/Encounter evidence/)).toBeTruthy();
    const before = screen.getByTestId('loom-url').textContent ?? '';
    expect(before).toContain('loomSession=');
    expect(before).toContain('loomEvent=m0');
    expect(before).toContain('loomEncounter=');

    await userEvent.click(screen.getByRole('button', { name: 'Return to encounters' }));

    const after = screen.getByTestId('loom-url').textContent ?? '';
    expect(after).toContain('loomSession=');
    expect(after).toContain('loomEvent=m0');
    expect(after).not.toContain('loomEncounter=');
  });

  it('draws the field from the typed Loom temporal read', async () => {
    const fetchMock = serve(HAPPY);
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
    render(
      <QueryClientProvider client={client}>
        <MemoryRouter><LoomPage /></MemoryRouter>
      </QueryClientProvider>,
    );
    await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' });
    const urls = fetchMock.mock.calls.map((call) => String(call[0]));
    expect(urls.some((url) => url.includes('/loom/temporal'))).toBe(true);
    expect(urls.some((url) => url.includes('/api/plugins/hermes-lcm/overview'))).toBe(false);
    // Three dated sessions become three lanes in both the field and its exact table.
    expect(document.querySelectorAll('[data-lane-row]')).toHaveLength(3);
    expect(document.querySelectorAll('[data-navigator-lane]')).toHaveLength(3);
    expect(screen.getByText('following loaded tail')).toBeTruthy();
  });

  it('projects a spawn curve only for an admitted same-provider parent, at the child start', async () => {
    const hierarchy = hierarchyFor(
      [treeNode('sess-open', null), treeNode('sess-closed', 'sess-open'), treeNode('sess-hollow', 'sess-open', 'codex'),
        treeNode('outside-loaded-page', 'sess-open', 'cursor', 'missing_parent'), treeNode('cycle', 'cycle', 'cursor', 'cycle')],
      { missing_parent_count: 1, cycle_count: 1, truncated: true },
    );
    renderLoom({
      ...HAPPY,
      '/api/loom/temporal': { status: 200, body: linkedTemporal(true) },
      '/api/plugins/analytics/subagent-tree': { status: 200, body: readyEnvelope(hierarchy) },
    });
    await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' });
    expect(screen.getByText(/Session hierarchy: partial/).textContent).toContain('1 missing parents · 1 cycles');
    const spawns = document.querySelectorAll('[data-event][data-kind="spawn"]');
    expect(spawns).toHaveLength(1);
    expect(spawns[0]!.getAttribute('data-grade')).toBe('inferred');
    expect(spawns[0]!.querySelector('title')?.textContent).toContain('tool-parent-7');
    expect(spawns[0]!.querySelector('title')?.textContent).toContain('fork placed at the child start');
    // The curve leaves the parent lane at the child's recorded start: the spawn
    // glyph and the child's session-start glyph share one x. Hit rects are
    // centred on the node, so equal centres means equal x + width/2.
    const childStart = document.querySelector(`[data-event="start:${LANE('cursor', 'sess-closed').replace(/"/g, '\\"')}"]`)!;
    const centre = (rect: Element) => Number(rect.getAttribute('x')) + Number(rect.getAttribute('width')) / 2;
    expect(centre(spawns[0]!.querySelector('rect')!)).toBeCloseTo(centre(childStart.querySelector('rect')!), 3);
    // The cross-provider child is not linked: no second curve, and the legend
    // names both the missing handoff authority and the truncated parentage.
    expect(screen.getByRole('list', { name: 'Evidence gaps' }).textContent).toContain('handoff unavailable');
    expect(screen.getByRole('list', { name: 'Evidence gaps' }).textContent).toContain('parentage unavailable');
  });

  it('grades a spawn ambiguous when the recorded child starts before its recorded parent', async () => {
    const hierarchy = hierarchyFor([treeNode('sess-open', null), treeNode('sess-closed', 'sess-open')]);
    renderLoom({
      ...HAPPY,
      '/api/loom/temporal': { status: 200, body: linkedTemporal() },
      '/api/plugins/analytics/subagent-tree': { status: 200, body: readyEnvelope(hierarchy) },
    });
    await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' });
    const spawn = document.querySelector('[data-event][data-kind="spawn"]')!;
    expect(spawn.getAttribute('data-grade')).toBe('ambiguous');
    expect(spawn.querySelector('title')?.textContent).toContain('child start precedes parent start');
  });

  it('collapses a branch into a bundle and keeps the URL, table and field in step', async () => {
    const hierarchy = hierarchyFor([treeNode('sess-open', null), treeNode('sess-closed', 'sess-open')]);
    renderLoom({
      ...HAPPY,
      '/api/loom/temporal': { status: 200, body: linkedTemporal() },
      '/api/plugins/analytics/subagent-tree': { status: 200, body: readyEnvelope(hierarchy) },
    });
    await screen.findByRole('button', { name: 'Collapse branch Deliver Git primitive runtime' });
    expect(document.querySelectorAll('[data-lane-row]')).toHaveLength(3);
    fireEvent.click(screen.getByRole('button', { name: 'Collapse branch Deliver Git primitive runtime' }));
    expect(screen.getByTestId('loom-url').textContent).toContain('loomCollapsed=');
    expect(document.querySelectorAll('[data-lane-row]')).toHaveLength(2);
    expect(document.querySelector('[data-cluster]')).not.toBeNull();
    // The exact table keeps the hidden child as a row and says where it went.
    expect(document.querySelectorAll('[data-navigator-lane]')).toHaveLength(3);
    expect(document.querySelectorAll('[data-navigator-hidden="true"]')).toHaveLength(1);
    expect(screen.getByText('in bundle')).toBeTruthy();
    // The bundle's count names its population: one descendant session.
    expect(screen.getByRole('button', { name: /Expand branch Deliver Git primitive runtime · 1 sessions/ })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Expand Deliver Git primitive runtime in navigator' }));
    expect(screen.getByTestId('loom-url').textContent).not.toContain('loomCollapsed=');
    expect(document.querySelectorAll('[data-lane-row]')).toHaveLength(3);
  });

  it('starts roots collapsed on a dense page and records an explicit expansion', async () => {
    const sessions = Array.from({ length: 60 }, (_, index) => ({
      ...TEMPORAL.payload.sessions[0]!,
      session_id: `dense-${index}`,
      title: `Dense ${index}`,
      started_at: NOW - 100_000 + index * 600,
      ended_at: NOW - 100_000 + index * 600 + 300,
      is_subagent: index % 2 === 1,
    }));
    const nodes = sessions.map((session, index) =>
      treeNode(session.session_id, index % 2 === 1 ? `dense-${index - 1}` : null),
    );
    renderLoom({
      ...HAPPY,
      '/api/loom/temporal': { status: 200, body: { ...TEMPORAL, payload: { ...TEMPORAL.payload, total: 60, sessions } } },
      '/api/plugins/analytics/subagent-tree': { status: 200, body: readyEnvelope(hierarchyFor(nodes)) },
    });
    await screen.findByRole('button', { name: 'Select session Dense 0' });
    expect(document.querySelectorAll('[data-cluster]')).toHaveLength(30);
    expect(document.querySelectorAll('[data-lane-row]')).toHaveLength(30);
    fireEvent.click(screen.getByRole('button', { name: 'Expand Dense 0 in navigator' }));
    expect(screen.getByTestId('loom-url').textContent).toContain('loomExpanded=');
    expect(document.querySelectorAll('[data-cluster]')).toHaveLength(29);
    expect(document.querySelectorAll('[data-lane-row]')).toHaveLength(31);
  });

  it('writes the time window to the URL and returns to the loaded tail', async () => {
    renderLoom();
    await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' });
    fireEvent.click(screen.getByRole('button', { name: 'Zoom in' }));
    expect(screen.getByTestId('loom-url').textContent).toContain('loomWindow=');
    expect(screen.queryByText('following loaded tail')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Return field to loaded tail' }));
    expect(screen.getByTestId('loom-url').textContent).not.toContain('loomWindow=');
    expect(screen.getByText('following loaded tail')).toBeTruthy();
  });

  it('hides an event kind through the filter without touching source truth', async () => {
    renderLoom();
    await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' });
    expect(document.querySelectorAll('[data-event][data-kind="commit"]')).toHaveLength(1);
    fireEvent.click(screen.getByRole('checkbox', { name: 'Show commit events' }));
    expect(screen.getByTestId('loom-url').textContent).toContain('loomHide=commit');
    expect(document.querySelectorAll('[data-event][data-kind="commit"]')).toHaveLength(0);
    expect(screen.getByText(/1 filtered/)).toBeTruthy();
    // The commit still exists in the exact evidence once its session is opened.
    await userEvent.click(screen.getByRole('button', { name: 'Select session Deliver Git primitive runtime' }));
    expect(await screen.findByText('abc123def456')).toBeTruthy();
  });

  it('uses the Loom temporal read for recorded ends and causal relations', async () => {
    renderLoom();
    await userEvent.click(await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' }));
    expect(await screen.findByText('src/runtime.rs')).toBeTruthy();
    expect(screen.getByText('abc123def456')).toBeTruthy();
    expect(screen.queryByText(/confidence/)).toBeNull();
    expect(screen.getByText('extent', { selector: 'dt' }).nextElementSibling?.textContent).toBe('30m');
    expect(screen.getByText(/1 active temporal generations/)).toBeTruthy();
    expect(screen.getByText(/complete coverage · 3 examined/)).toBeTruthy();
    expect(screen.getByTestId('loom-url').textContent).toContain('loomSession=');
  });

  it('does not present unfed Delivery outcomes as a Loom relation', async () => {
    renderLoom();
    await userEvent.click(await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' }));
    expect(screen.queryByText('→ delivery outcomes')).toBeNull();
  });

  it('reports each causal source with its real authority or dependency', async () => {
    renderLoom();
    await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' });
    expect(screen.getByText('Session ↔ commit')).toBeTruthy();
    expect(screen.getByText('Session → edited file')).toBeTruthy();
    expect(screen.getByText('Branch & worktree spans')).toBeTruthy();
    expect(screen.getByText(/commit_sessions/)).toBeTruthy();
  });

  it('reports canonical temporal retrieval as unavailable on selection', async () => {
    renderLoom();
    await userEvent.click(await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' }));
    expect(await screen.findByText(/lcm_temporal_retrieval_not_mounted/)).toBeTruthy();
    expect(screen.queryByText('ordinal order')).toBeNull();
  });

  it('renders chain token provenance and keeps unknown counts unknown', async () => {
    renderLoom({ ...HAPPY, '/api/plugins/hermes-lcm/session/': { status: 200, body: readyEnvelope(CHAIN) } });
    await userEvent.click(await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' }));
    expect(await screen.findByText('~12 tokens · o200k approximate')).toBeTruthy();
    expect(screen.getByText('~20 tokens · o200k approximate')).toBeTruthy();
    expect(screen.getByText('tokens unknown')).toBeTruthy();
  });

  it('draws undated turns in recorded order and shares one reveal cursor across field, rows and URL', async () => {
    renderLoom({ ...HAPPY, '/api/plugins/hermes-lcm/session/': { status: 200, body: readyEnvelope(CHAIN) } });
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' }));
    await screen.findByText('stored ordinal 2');
    const lane = LANE('cursor', 'sess-open');
    const node = (id: string) => document.querySelector(`[data-event="msg:${lane.replace(/"/g, '\\"')}:${id}"]`);
    expect(node('m2')).not.toBeNull();
    expect(node('m2')!.getAttribute('data-x-basis')).toBe('sequence');
    expect(screen.getByRole('img', { name: /undated events/ })).toBeTruthy();
    expect(document.querySelector('[data-cursor]')).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Select user message user' }));
    expect(screen.getByTestId('loom-url').textContent).toContain('loomEvent=m0');
    expect(screen.getByTestId('loom-url').textContent).toContain('scope=project-loom');
    expect(node('m2')).toBeNull();
    expect(node('m1')).toBeNull();
    expect(document.querySelector('[data-cursor]')).not.toBeNull();
    expect(screen.queryByText('Running the suite.')).toBeNull();
    expect(screen.queryByText('Bash')).toBeNull();
    expect(screen.queryByText('src/runtime.rs')).toBeNull();
    expect(screen.queryByText('abc123def456')).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Step to next stored event' }));
    expect(node('m1')).not.toBeNull();
    expect(screen.getByRole('button', { name: 'Inspect stored event m1' })).toBeTruthy();
    fireEvent.keyDown(screen.getByRole('button', { name: 'Select user message user' }), { key: 'Enter' });
    expect(screen.getByText('stored ordinal 0')).toBeTruthy();
    await user.click(screen.getByRole('button', { name: 'Return replay to latest loaded event' }));
    expect(node('m2')).not.toBeNull();
    expect(screen.getByTestId('loom-url').textContent).not.toContain('loomEvent');
    await user.click(screen.getByRole('button', { name: '← All loaded sessions' }));
    expect(screen.getByTestId('loom-url').textContent).not.toContain('loomSession');
    expect(document.querySelectorAll('[data-lane-row]')).toHaveLength(3);
  });

  it('withholds later dated records on every lane once a dated cursor is set', async () => {
    const dated = {
      ...CHAIN,
      messages: [
        chainMessage({ message_id: 'm0', role: 'user', content: 'first', ordinal: 0, timestamp: NOW - 7100 }),
        chainMessage({ message_id: 'm1', role: 'assistant', content: 'second', ordinal: 1, timestamp: NOW - 6000 }),
        chainMessage({ message_id: 'm2', role: 'assistant', content: 'third', ordinal: 2, timestamp: NOW - 4000 }),
      ],
    };
    renderLoom({ ...HAPPY, '/api/plugins/hermes-lcm/session/': { status: 200, body: readyEnvelope(dated) } });
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' }));
    await screen.findByText('stored ordinal 2');
    // The commit at NOW-5000 and the codex session starting at NOW-3600 are both drawn while following.
    expect(document.querySelectorAll('[data-event][data-kind="commit"]')).toHaveLength(1);
    expect(document.querySelector(`[data-event="start:${LANE('codex', 'sess-hollow').replace(/"/g, '\\"')}"]`)).not.toBeNull();
    await user.click(screen.getByRole('button', { name: 'Select user message user' }));
    // Cursor at NOW-7100: the later commit and the later session start are unrevealed, not dimmed.
    expect(document.querySelectorAll('[data-event][data-kind="commit"]')).toHaveLength(0);
    expect(document.querySelector(`[data-event="start:${LANE('codex', 'sess-hollow').replace(/"/g, '\\"')}"]`)).toBeNull();
    expect(document.querySelector('[data-scene-counts]')!.textContent).toMatch(/[1-9]\d* withheld/);
  });

  it('follows appended admitted page members but retains or discloses an inspected identity on refetch', async () => {
    const routes = { ...HAPPY, '/api/plugins/hermes-lcm/session/': { status: 200, body: readyEnvelope(CHAIN) } };
    const { client } = renderLoom(routes);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' }));
    await screen.findByText('stored ordinal 2');
    const appended = [...CHAIN.messages, chainMessage({ message_id: 'm3', ordinal: 3, content: 'New admitted page member' })];
    routes['/api/plugins/hermes-lcm/session/'].body = readyEnvelope({ ...CHAIN, messages: appended });
    await act(() => client.invalidateQueries({ queryKey: ['loom', 'chain'] }));
    await screen.findByText('stored ordinal 3');
    await user.click(screen.getByRole('button', { name: 'Inspect stored event m1' }));
    routes['/api/plugins/hermes-lcm/session/'].body = readyEnvelope({ ...CHAIN, messages: appended.slice(1) });
    await act(() => client.invalidateQueries({ queryKey: ['loom', 'chain'] }));
    expect(screen.getByText('stored ordinal 1')).toBeTruthy();
    expect(screen.getByTestId('loom-url').textContent).toContain('loomEvent=m1');
    routes['/api/plugins/hermes-lcm/session/'].body = readyEnvelope({ ...CHAIN, messages: appended.slice(2) });
    await act(() => client.invalidateQueries({ queryKey: ['loom', 'chain'] }));
    expect(await screen.findByText(/Selected event m1 is outside this loaded page/)).toBeTruthy();
    expect(document.querySelectorAll('[data-event][data-kind^="message"], [data-event][data-kind="tool_call"]')).toHaveLength(0);
    expect(screen.queryByText('New admitted page member')).toBeNull();
    expect(screen.getByTestId('loom-url').textContent).toContain('loomEvent=m1');
  });

  it('replays only the canonical loaded raw-turn order and keeps compaction linked', async () => {
    const user = userEvent.setup();
    renderLoom({
      ...HAPPY,
      '/api/plugins/hermes-lcm/session/': {
        status: 200,
        body: readyEnvelope({
          ...CHAIN,
          has_more: true,
          has_more_messages: true,
          next_cursor: 'opaque-next-page',
          summary_nodes: [
            {
              category: 'checkpoint', created_at: NOW - 100, depth: 1,
              expand_hint: 'open the canonical transcript page', latest_at: null,
              node_id: 'summary-raw-0', recency: null, session_id: 'sess-open',
              snippet: 'compacted setup', source_token_count: 30, source_type: 'message',
              summary: 'The setup was compacted.', token_count: 8,
            },
          ],
          messages: [
            chainMessage({ message_id: 'm2', role: 'assistant', content: 'Running the suite.', ordinal: 2, timestamp: NOW - 60 }),
            chainMessage({ message_id: 'm0', role: 'user', content: 'Verify durable code-generation restart.', ordinal: 0, timestamp: NOW - 120, summary_node_ids: ['summary-raw-0'] }),
            chainMessage({ message_id: 'm1', role: 'assistant', content: 'Reading the reconciliation path.', ordinal: 1, timestamp: NOW - 90 }),
          ],
        }),
      },
    });

    await user.click(await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' }));
    expect(await screen.findByText('stored ordinal 2')).toBeTruthy();
    expect(screen.getByText('following loaded tail', { selector: 'span.px-1' })).toBeTruthy();
    expect(screen.getByText(/later pages remain outside this replay/)).toBeTruthy();

    await user.click(screen.getByRole('button', { name: 'Step to previous stored event' }));
    expect(screen.getByText('stored ordinal 1')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Return replay to latest loaded event' })).toBeTruthy();

    await user.click(screen.getByRole('button', { name: 'Step to previous stored event' }));
    expect(screen.getByText('stored ordinal 0')).toBeTruthy();
    expect(screen.getByText('linked compaction boundaries')).toBeTruthy();
    expect(screen.queryByText(/checkpoint · depth 1/)).toBeNull();
    expect(screen.getByText(/linked boundary is outside this loaded transcript page/)).toBeTruthy();

    await user.selectOptions(screen.getByLabelText('Replay speed'), '2');
    expect((screen.getByLabelText('Replay speed') as HTMLSelectElement).value).toBe('2');
    await user.click(screen.getByRole('button', { name: 'Play replay' }));
    expect(screen.getByRole('button', { name: 'Pause replay' })).toBeTruthy();
    await user.click(screen.getByRole('button', { name: 'Pause replay' }));
    await user.click(screen.getByRole('button', { name: 'Return replay to latest loaded event' }));
    expect(screen.getByText('following loaded tail', { selector: 'span.px-1' })).toBeTruthy();
  });

  it('does not render partial commit coverage as a zero result', async () => {
    const sourceStatuses = TEMPORAL.payload.source_statuses.map((source) =>
      source.id === 'session_commit'
        ? {
            ...source,
            state: 'partial',
            reason: 'one providerless legacy attribution was omitted',
            coverage: { ...source.coverage, completeness: 'partial', eligible: 1, examined: 1, matched: 0, omitted: 1 },
          }
        : source,
    );
    renderLoom({
      ...HAPPY,
      '/api/loom/temporal': {
        status: 200,
        body: { ...TEMPORAL, payload: { ...TEMPORAL.payload, commits: [], source_statuses: sourceStatuses } },
      },
    });

    await userEvent.click(await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' }));
    expect(screen.getAllByText(/one providerless legacy attribution was omitted/).length).toBeGreaterThanOrEqual(1);
    expect(screen.queryByText(/commit_sessions has no attribution for this session/)).toBeNull();
  });

  it('distinguishes a store that reports itself unavailable from an empty one', async () => {
    renderLoom({
      '/api/loom/temporal': {
        status: 200,
        body: { ...TEMPORAL, payload: { ...TEMPORAL.payload, available: false, total: 0, sessions: [] } },
      },
      '/api/plugins/hermes-lcm/timeline': { status: 200, body: readyEnvelope(TIMELINE) },
    });
    await screen.findByText(/reported its session store unavailable/);
    expect(screen.queryByText(/No thread to weave/)).toBeNull();
  });

  it('renders the empty field as an answered question when the store is genuinely empty', async () => {
    renderLoom({
      '/api/loom/temporal': { status: 200, body: { ...TEMPORAL, payload: { ...TEMPORAL.payload, total: 0, sessions: [] } } },
      '/api/plugins/hermes-lcm/timeline': { status: 200, body: readyEnvelope(TIMELINE) },
    });
    await screen.findByText('No thread to weave');
    expect(screen.getByText(/answered and holds no sessions in this scope/)).toBeTruthy();
  });

  it('keeps dated sessions visible when the backend also returns an undated row', async () => {
    renderLoom({
      '/api/loom/temporal': {
        status: 200,
        body: {
          ...TEMPORAL,
          payload: {
            ...TEMPORAL.payload,
            sessions: [TEMPORAL.payload.sessions[0], { ...TEMPORAL.payload.sessions[2], session_id: 'sess-undated', started_at: null }],
          },
        },
      },
      '/api/plugins/hermes-lcm/timeline': { status: 200, body: readyEnvelope(TIMELINE) },
    });

    await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' });
    expect(screen.getByText(/1 row carried no usable start time/)).toBeTruthy();
    expect(screen.queryByText(/Loom temporal response unavailable/)).toBeNull();
  });

  it('renders a distinct error state when the read fails, inventing nothing', async () => {
    renderLoom({ '/api/loom/temporal': { status: 500, body: { error: 'boom' } } });
    await waitFor(() => {
      expect(screen.getByText(/HTTP 500/)).toBeTruthy();
    });
    expect(screen.queryByText('No thread to weave')).toBeNull();
  });

  it('survives a timeline read failing without losing the field', async () => {
    renderLoom({
      '/api/loom/temporal': { status: 200, body: TEMPORAL },
      '/api/plugins/hermes-lcm/timeline': { status: 500, body: { error: 'boom' } },
    });
    await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' });
    expect(screen.getByText('timeline read failed')).toBeTruthy();
  });

  it('gives the field an accessible description, a legend and a real table alongside', async () => {
    renderLoom();
    await screen.findByRole('button', { name: 'Select session Deliver Git primitive runtime' });
    const figure = screen.getByRole('region', { name: /Temporal execution field:/ });
    expect(figure.getAttribute('aria-label')).toContain('drawn open');
    expect(figure.getAttribute('aria-label')).toContain('handoff and result remain unavailable');
    expect(screen.getByRole('table')).toBeTruthy();
    for (const grade of ['EXACT', 'EXPLICIT', 'INFERRED', 'AMBIGUOUS', 'STALE', 'UNAVAILABLE']) {
      expect(screen.getByText(grade)).toBeTruthy();
    }
    expect(screen.getByText('NOW = newest record in this loaded page · not a live stream')).toBeTruthy();
  });
});
