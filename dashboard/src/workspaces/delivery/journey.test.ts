import { describe, expect, it } from 'vitest';
import type { DeliveryOverviewV1 } from '../../contracts/generated.ts';
import { INBOX, OVERVIEW_ALPHA, OVERVIEW_LOCAL_ONLY, T0 } from '../../test/deliveryFixtures.ts';
import { JOURNEY_LANES, buildJourney, laneServes } from './journey.ts';

const ROW_42 = INBOX.pull_requests[0]!;
const EDGES_42 = INBOX.membership_edges.filter(
  (edge) => edge.project_id === 'project.alpha' && edge.pull_request_id === '42',
);

describe('buildJourney', () => {
  it('joins every lane from its own authority with source class and grade', () => {
    const model = buildJourney(OVERVIEW_ALPHA, { row: ROW_42, edges: EDGES_42 });
    expect(model.lanes.map((lane) => lane.id)).toEqual([...JOURNEY_LANES]);
    const byLane = new Map(model.lanes.map((lane) => [lane.id, lane]));

    expect(byLane.get('commits')!.episodes.map((e) => [e.source, e.grade, e.timeKind])).toEqual([
      ['commit', 'exact', 'event'],
      ['commit', 'exact', 'event'],
    ]);
    expect(byLane.get('reviews')!.episodes.map((e) => e.label)).toEqual([
      'src/ingest/retry.ts:142',
      'src/ingest/config.ts',
    ]);
    expect(byLane.get('reviews')!.episodes.every((e) => e.timeKind === 'observed')).toBe(true);
    expect(byLane.get('checks')!.episodes.map((e) => e.status)).toEqual(['ready', 'error']);
    expect(byLane.get('pull_request')!.episodes[0]).toMatchObject({
      source: 'pull_request',
      grade: 'exact',
      timeKind: 'observed',
    });
  });

  it('places membership-joined records in the undated gutter with their own grade', () => {
    const model = buildJourney(OVERVIEW_ALPHA, { row: ROW_42, edges: EDGES_42 });
    const byLane = new Map(model.lanes.map((lane) => [lane.id, lane]));
    expect(byLane.get('objective')!.episodes).toHaveLength(1);
    expect(byLane.get('objective')!.episodes[0]).toMatchObject({
      grade: 'explicit',
      at: null,
      timeKind: 'undated',
      href: null,
    });
    expect(byLane.get('sessions')!.episodes[0]).toMatchObject({
      grade: 'inferred',
      at: null,
      href: '/loom?loomSession=session.alpha.1',
    });
    // Objective, session, and the two agent-usage rows carry no event time.
    expect(model.undated).toBe(4);
  });

  it('keeps an unjoined lane as a typed absence, not an empty success', () => {
    const model = buildJourney(
      {
        ...OVERVIEW_ALPHA,
        agent_usage: {
          state: 'not_published',
          reason: 'no session has recorded a Git branch span yet',
          required_authority: 'session-Git correlation index',
        },
      },
      { row: ROW_42, edges: [] },
    );
    const agents = model.lanes.find((lane) => lane.id === 'agents')!;
    expect(agents.state).toMatchObject({
      kind: 'not_published',
      requiredAuthority: 'session-Git correlation index',
    });
    expect(laneServes(agents.state)).toBe(false);
    expect(agents.episodes).toEqual([]);
    const releases = model.lanes.find((lane) => lane.id === 'releases')!;
    expect(releases.state).toMatchObject({
      kind: 'not_published',
      requiredAuthority: 'github_read_authority',
    });
  });

  it("puts per-agent token and tool-call counts on the pull request's branch", () => {
    const model = buildJourney(OVERVIEW_ALPHA, { row: ROW_42, edges: [] });
    const agents = model.lanes.find((lane) => lane.id === 'agents')!;
    expect(agents.state.kind).toBe('served');
    expect(agents.episodes.map((episode) => [episode.label, episode.detail, episode.grade])).toEqual([
      ['planner', 'claude · 2 sessions · 21,500 tokens · 41 tool calls', 'inferred'],
      // A session without provider usage keeps its tool calls and says so,
      // rather than printing zero tokens.
      ['Unattributed codex sessions', 'codex · 1 session · tokens not reported · 6 tool calls', 'inferred'],
    ]);
  });

  it('does not attribute usage read for another branch to this pull request', () => {
    const model = buildJourney(OVERVIEW_ALPHA, {
      row: { ...ROW_42, branch_ref: 'refs/heads/feature/retry' },
      edges: [],
    });
    const agents = model.lanes.find((lane) => lane.id === 'agents')!;
    expect(agents.episodes).toEqual([]);
    expect(agents.state.kind).toBe('unavailable');
    expect(agents.state.detail).toMatch(/read for the checkout's branch feature\/delivery, not this pull request's head feature\/retry/);
  });

  it('names why agent usage is partial', () => {
    const usage = OVERVIEW_ALPHA.agent_usage.state === 'ready' ? OVERVIEW_ALPHA.agent_usage.value : null;
    const model = buildJourney(
      {
        ...OVERVIEW_ALPHA,
        agent_usage: { state: 'partial', value: { ...usage!, usage_coverage: 'unavailable', truncated: true } },
      },
      { row: ROW_42, edges: [] },
    );
    const agents = model.lanes.find((lane) => lane.id === 'agents')!;
    expect(agents.state).toEqual({
      kind: 'partial',
      detail:
        'Agent usage: the correlation read reached its session ceiling; provider usage coverage is unavailable, so token counts are lower bounds',
    });
    expect(agents.episodes).toHaveLength(2);
  });

  it('reports the selected pull request missing from the head-bound page as a gap', () => {
    const model = buildJourney(OVERVIEW_ALPHA, {
      row: INBOX.pull_requests[1]!,
      edges: [],
    });
    expect(model.gaps).toHaveLength(1);
    expect(model.gaps[0]).toMatch(/#43 is not among the 1 head-bound provider items/);
    expect(model.lanes.find((lane) => lane.id === 'pull_request')!.episodes).toEqual([]);
  });

  it('marks stale projections stale rather than exact', () => {
    const stale: DeliveryOverviewV1 = {
      ...OVERVIEW_ALPHA,
      commits: { state: 'stale', value: OVERVIEW_ALPHA.commits.state === 'ready' ? OVERVIEW_ALPHA.commits.value : { items: [], truncated: false } },
    };
    const model = buildJourney(stale, { row: ROW_42, edges: [] });
    const commits = model.lanes.find((lane) => lane.id === 'commits')!;
    expect(commits.state.kind).toBe('stale');
    expect(commits.episodes.every((episode) => episode.grade === 'stale')).toBe(true);
  });

  it('leaves provider lanes typed when only local Git is available', () => {
    const model = buildJourney(OVERVIEW_LOCAL_ONLY, { row: ROW_42, edges: [] });
    const kinds = Object.fromEntries(model.lanes.map((lane) => [lane.id, lane.state.kind]));
    expect(kinds).toMatchObject({
      commits: 'served',
      pull_request: 'not_published',
      reviews: 'not_published',
      checks: 'not_published',
      releases: 'not_published',
    });
    expect(model.span).toEqual({ start: T0 + 3_600_000_000, end: T0 + 3 * 3_600_000_000 });
  });
});
