import { describe, expect, it } from 'vitest';
import type { DeliveryOverviewV1 } from '../../contracts/generated.ts';
import { INBOX, OVERVIEW_ALPHA, OVERVIEW_LOCAL_ONLY, T0 } from '../../test/deliveryFixtures.ts';
import {
  JOURNEY_GUTTER_WIDTH,
  JOURNEY_LANES,
  buildJourney,
  laneServes,
  layoutJourney,
} from './journey.ts';

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
    expect(model.undated).toBe(2);
  });

  it('keeps an unjoined lane as a typed absence, not an empty success', () => {
    const model = buildJourney(OVERVIEW_ALPHA, { row: ROW_42, edges: [] });
    const agents = model.lanes.find((lane) => lane.id === 'agents')!;
    expect(agents.state.kind).toBe('unavailable');
    expect(laneServes(agents.state)).toBe(false);
    const releases = model.lanes.find((lane) => lane.id === 'releases')!;
    expect(releases.state).toMatchObject({
      kind: 'not_published',
      requiredAuthority: 'github_read_authority',
    });
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

describe('layoutJourney', () => {
  it('is deterministic and never invents a timestamp for an undated record', () => {
    const model = buildJourney(OVERVIEW_ALPHA, { row: ROW_42, edges: EDGES_42 });
    const first = layoutJourney(model, { width: 900 });
    const second = layoutJourney(model, { width: 900 });
    expect(second).toEqual(first);
    expect(first.gutterWidth).toBe(JOURNEY_GUTTER_WIDTH);
    for (const point of first.points) {
      if (point.episode.at === null) {
        expect(point.x).toBeLessThan(first.gutterWidth);
      } else {
        expect(point.x).toBeGreaterThanOrEqual(first.gutterWidth);
      }
    }
  });

  it('orders dated points by recorded time and keeps lane rows fixed', () => {
    const model = buildJourney(OVERVIEW_ALPHA, { row: ROW_42, edges: [] });
    const layout = layoutJourney(model, { width: 900 });
    expect(layout.gutterWidth).toBe(0);
    const dated = layout.points
      .filter((point) => point.episode.at !== null)
      .sort((left, right) => (left.episode.at as number) - (right.episode.at as number));
    for (let index = 1; index < dated.length; index += 1) {
      expect(dated[index]!.x).toBeGreaterThanOrEqual(dated[index - 1]!.x);
    }
    const commitsRow = layout.rows.find((row) => row.lane.id === 'commits')!;
    for (const point of layout.points.filter((p) => p.episode.lane === 'commits')) {
      expect(point.y).toBe(commitsRow.y);
    }
    expect(layout.ticks.length).toBeGreaterThan(1);
    expect(layout.ticks.length).toBeLessThanOrEqual(9);
  });

  it('fans out same-instant marks on one lane instead of overprinting them', () => {
    const layout = layoutJourney(buildJourney(OVERVIEW_ALPHA, { row: ROW_42, edges: [] }), { width: 900 });
    const reviews = layout.points.filter((point) => point.episode.lane === 'reviews');
    const row = layout.rows.find((candidate) => candidate.lane.id === 'reviews')!;
    expect(reviews).toHaveLength(2);
    expect(reviews[0]!.x).toBe(reviews[1]!.x);
    expect(new Set(reviews.map((point) => point.y)).size).toBe(2);
    expect(reviews.map((point) => Math.abs(point.y - row.y)).sort()).toEqual([0, 9]);
    for (const tick of layout.ticks.slice(1)) {
      const previous = layout.ticks[layout.ticks.indexOf(tick) - 1]!;
      expect(tick.x - previous.x).toBeGreaterThanOrEqual(44);
    }
  });

  it('compresses long empty time into a visible break', () => {
    const late: DeliveryOverviewV1 = {
      ...OVERVIEW_ALPHA,
      releases: {
        state: 'ready',
        value: {
          truncated: false,
          items: [
            {
              id: 'release.v1',
              label: 'v1.0.0',
              name: 'v1.0.0',
              tag: 'v1.0.0',
              draft: false,
              prerelease: false,
              release_id: 1,
              source_url: 'https://github.com/example/alpha/releases/v1.0.0',
              created_at_micros: T0 + 400 * 3_600_000_000,
              published_at_micros: T0 + 400 * 3_600_000_000,
              assets: [],
            },
          ],
        },
      },
    };
    const layout = layoutJourney(buildJourney(late, { row: ROW_42, edges: [] }), { width: 900 });
    expect(layout.breaks).toHaveLength(1);
    expect(layout.breaks[0]!.toMicros).toBe(T0 + 400 * 3_600_000_000);
  });
});
