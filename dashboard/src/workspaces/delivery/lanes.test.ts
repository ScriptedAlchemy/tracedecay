import { describe, expect, it } from 'vitest';
import type { DeliveryInboxV1 } from '../../contracts/generated.ts';
import { INBOX, OVERVIEW_ALPHA } from '../../test/deliveryFixtures.ts';
import { edgesFor } from './inboxFilter.ts';
import { buildJourney } from './journey.ts';
import { DENSE_LANE_LIMIT, laneZoom, layoutLanes } from './lanes.ts';
import { buildUmbrellas } from './umbrella.ts';

const VIEWPORT = { width: 900, height: 520 };
const PORTFOLIO = { zoom: 'portfolio' as const, project: null, pullRequest: null, journey: null };

describe('laneZoom', () => {
  it('maps the URL scope onto the semantic zoom level', () => {
    expect(laneZoom(null, null)).toBe('portfolio');
    expect(laneZoom('project.alpha', null)).toBe('repository');
    expect(laneZoom(null, 'project.alpha:github:42')).toBe('pull_request');
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
    expect(layout.threads.map((thread) => thread.link.code)).toEqual(['WORK', 'AGENT']);
    expect(layout.hiddenLinks).toBe(0);
  });

  it('compresses lanes outside the focused repository and counts links it hides', () => {
    const rows = INBOX.pull_requests.filter((row) => row.project_id === 'project.alpha');
    const layout = layoutLanes(INBOX, rows, buildUmbrellas(INBOX), VIEWPORT, {
      zoom: 'repository',
      project: 'project.alpha',
      pullRequest: null,
      journey: null,
    });
    expect(layout.lanes.map((lane) => [lane.project.project_id, lane.compressed, lane.bars.length])).toEqual([
      ['project.alpha', false, 2],
      ['project.beta', true, 0],
    ]);
    expect(layout.threads).toEqual([]);
    expect(layout.hiddenLinks).toBe(2);
  });

  it('expands the focused PR into provider, attention and journey tracks with typed gaps', () => {
    const row = INBOX.pull_requests[0]!;
    const edges = edgesFor(INBOX, row);
    const layout = layoutLanes(INBOX, INBOX.pull_requests, buildUmbrellas(INBOX), VIEWPORT, {
      zoom: 'pull_request',
      project: null,
      pullRequest: row.id,
      journey: buildJourney({ ...OVERVIEW_ALPHA, ci_checks: { state: 'denied', value: null } }, { row, edges }),
    });
    const bar = layout.lanes[0]!.bars.find((candidate) => candidate.row.id === row.id)!;
    expect(bar.tracks.map((track) => [track.id, track.beads.length, track.absence])).toEqual([
      ['provider_read', 1, null],
      ['attention', 2, null],
      ['commits', 2, null],
      ['reviews', 2, null],
      ['checks', 0, 'denied · Checks refused for this identity'],
    ]);
    expect(layout.lanes[1]!.compressed).toBe(true);
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
    const layout = layoutLanes(dense, dense.pull_requests, buildUmbrellas(dense), VIEWPORT, PORTFOLIO);
    const beta = layout.lanes[1]!;
    expect([beta.compressed, beta.summary.admitted, beta.bars.length, beta.bins.reduce((sum, bin) => sum + bin.count, 0)]).toEqual([
      true,
      9,
      0,
      9,
    ]);
  });

  it('prints the provider reason across a repository with no admitted PR', () => {
    const inbox: DeliveryInboxV1 = {
      ...INBOX,
      projects: [...INBOX.projects, { ...INBOX.projects[0]!, project_id: 'project.gamma', label: 'gamma', repository_id: 'repository.project.gamma', provider_state: 'not_published' }],
      omitted_projects: 1,
    };
    const layout = layoutLanes(inbox, inbox.pull_requests, buildUmbrellas(inbox), VIEWPORT, PORTFOLIO);
    expect(layout.lanes.map((lane) => lane.absence)).toEqual([null, null, 'not_published · requires github_read_authority.']);
    expect(layout.omitted?.count).toBe(1);
  });
});
