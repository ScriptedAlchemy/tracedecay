import { describe, expect, it } from 'vitest';
import type { DeliveryInboxV1 } from '../../contracts/generated.ts';
import { INBOX } from '../../test/deliveryFixtures.ts';
import { DENSE_LANE_LIMIT, laneZoom, layoutLanes, zoomPatch } from './lanes.ts';
import { buildUmbrellas } from './umbrella.ts';

const VIEWPORT = { width: 900, height: 520 };
const PORTFOLIO = { zoom: 'portfolio' as const, project: null, pullRequest: null };

describe('laneZoom', () => {
  it('maps the URL scope onto the semantic zoom level and back', () => {
    expect(laneZoom(null, null)).toBe('portfolio');
    expect(laneZoom('project.alpha', null)).toBe('repository');
    expect(laneZoom(null, 'project.alpha:github:42')).toBe('pull_request');
    expect(zoomPatch('portfolio', 'project.alpha')).toEqual({ project: null, pullRequest: null });
    expect(zoomPatch('repository', 'project.alpha')).toEqual({ project: 'project.alpha', pullRequest: null });
    expect(zoomPatch('pull_request', 'project.alpha')).toEqual({});
  });
});

describe('layoutLanes', () => {
  it('draws one lane per repository with exact counts and observation-window bars', () => {
    const layout = layoutLanes(INBOX, INBOX.pull_requests, buildUmbrellas(INBOX), VIEWPORT, PORTFOLIO);
    expect(layout.lanes.map((lane) => [lane.project.project_id, lane.compressed, lane.summary])).toEqual([
      ['project.alpha', false, { admitted: 2, drawn: 2, active: 2, stale: 0, correlated: 2 }],
      ['project.beta', false, { admitted: 1, drawn: 1, active: 0, stale: 1, correlated: 1 }],
    ]);
    expect(layout.lanes.flatMap((lane) => lane.bars.map((bar) => [bar.row.pull_request.pull_request_id, bar.undated, bar.beads.map((bead) => bead.kind)]))).toEqual([
      ['42', false, ['provider_read', 'attention', 'attention']],
      ['43', true, []],
      ['8', false, ['provider_read']],
    ]);
    expect(layout.threads.map((thread) => [thread.link.code, thread.link.grade])).toEqual([
      ['WORK', 'explicit'],
      ['AGENT', 'inferred'],
    ]);
    expect(layout.hiddenLinks).toBe(0);
  });

  it('gives every PR a 44px row band, however thin the drawn bar', () => {
    const layout = layoutLanes(INBOX, INBOX.pull_requests, buildUmbrellas(INBOX), VIEWPORT, PORTFOLIO);
    expect(layout.rowHeight).toBe(44);
    const ys = layout.lanes[0]!.bars.map((bar) => bar.y);
    expect(ys[1]! - ys[0]!).toBe(44);
  });

  it('ramps recency over the loaded page: the newest observation is 1, the oldest 0', () => {
    const layout = layoutLanes(INBOX, INBOX.pull_requests, buildUmbrellas(INBOX), VIEWPORT, PORTFOLIO);
    const bars = layout.lanes.flatMap((lane) => lane.bars);
    expect(bars.map((bar) => [bar.row.pull_request.pull_request_id, bar.recency])).toEqual([
      ['42', 1],
      ['43', 0],
      ['8', 0],
    ]);
    expect(bars[0]!.beads.map((bead) => bead.recency)).toEqual([0, 1, 1]);
  });

  it('compresses lanes outside the focused repository and counts links it hides', () => {
    const rows = INBOX.pull_requests.filter((row) => row.project_id === 'project.alpha');
    const layout = layoutLanes(INBOX, rows, buildUmbrellas(INBOX), VIEWPORT, { zoom: 'repository', project: 'project.alpha', pullRequest: null });
    expect(layout.lanes.map((lane) => [lane.project.project_id, lane.compressed, lane.bars.length])).toEqual([
      ['project.alpha', false, 2],
      ['project.beta', true, 0],
    ]);
    expect(layout.threads).toEqual([]);
    expect(layout.hiddenLinks).toBe(2);
  });

  it('expands the focused PR into provider-read and attention tracks with typed gaps', () => {
    const [pr42, pr43] = INBOX.pull_requests;
    const focused = layoutLanes(INBOX, INBOX.pull_requests, buildUmbrellas(INBOX), VIEWPORT, { zoom: 'pull_request', project: null, pullRequest: pr42!.id });
    expect(focused.lanes[0]!.bars[0]!.tracks.map((track) => [track.id, track.beads.map((bead) => bead.kind), track.absence])).toEqual([
      ['provider_read', ['provider_read'], null],
      ['attention', ['attention', 'attention'], null],
    ]);
    expect(focused.lanes[1]!.compressed).toBe(true);
    const bare = layoutLanes(INBOX, INBOX.pull_requests, buildUmbrellas(INBOX), VIEWPORT, { zoom: 'pull_request', project: null, pullRequest: pr43!.id });
    expect(bare.lanes[0]!.bars[1]!.tracks.map((track) => track.absence)).toEqual(['no provider read snapshot served', 'no attention served']);
  });

  it(`summarizes a portfolio lane of more than ${DENSE_LANE_LIMIT} PRs as a density ribbon`, () => {
    const template = INBOX.pull_requests[2]!;
    const dense: DeliveryInboxV1 = {
      ...INBOX,
      pull_requests: [
        ...INBOX.pull_requests,
        ...Array.from({ length: DENSE_LANE_LIMIT }, (_, index) => ({
          ...template,
          id: `project.beta:github:${100 + index}`,
          pull_request: { ...template.pull_request, pull_request_id: String(100 + index) },
        })),
      ],
    };
    const beta = layoutLanes(dense, dense.pull_requests, buildUmbrellas(dense), VIEWPORT, PORTFOLIO).lanes[1]!;
    expect([beta.compressed, beta.summary.admitted, beta.bars.length, beta.bins.reduce((sum, bin) => sum + bin.count, 0)]).toEqual([true, 9, 0, 9]);
  });

  it('prints the provider reason across a repository with no admitted PR and a scope note on a filtered one', () => {
    const inbox: DeliveryInboxV1 = {
      ...INBOX,
      projects: [...INBOX.projects, { ...INBOX.projects[0]!, project_id: 'project.gamma', label: 'gamma', repository_id: 'repository.project.gamma', provider_state: 'not_published' }],
      omitted_projects: 1,
    };
    const rows = inbox.pull_requests.filter((row) => row.project_id === 'project.alpha');
    const layout = layoutLanes(inbox, rows, buildUmbrellas(inbox), VIEWPORT, PORTFOLIO);
    expect(layout.lanes.map((lane) => lane.absence)).toEqual([
      null,
      '1 admitted · none in the current scope',
      'not_published · requires github_read_authority.',
    ]);
    expect(layout.omitted?.count).toBe(1);
  });
});
