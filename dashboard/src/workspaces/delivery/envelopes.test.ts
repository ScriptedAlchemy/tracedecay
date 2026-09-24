import { describe, expect, it } from 'vitest';
import { INBOX, INBOX_BRANCH_ONLY } from '../../test/deliveryFixtures.ts';
import { layoutEnvelopes } from './envelopes.ts';
import { buildUmbrellas } from './umbrella.ts';

const VIEWPORT = { width: 720, height: 520 };

describe('layoutEnvelopes', () => {
  it('places repositories on a canonical-id grid with one tracked-head station each', () => {
    const layout = layoutEnvelopes(INBOX, INBOX.pull_requests, buildUmbrellas(INBOX), VIEWPORT);
    expect(layout.envelopes.map((envelope) => [envelope.project.project_id, envelope.x, envelope.y, envelope.admitted])).toEqual([
      ['project.alpha', 16, 16, 2],
      ['project.beta', 369, 16, 1],
    ]);
    expect(layout.envelopes.map((envelope) => envelope.stations.map((station) => [station.branchRef, station.tracked]))).toEqual([
      [['refs/heads/feature/delivery', true]],
      [['refs/heads/feature/retry', true]],
    ]);
    expect(layout.omitted).toBeNull();
  });

  it('sizes marks by served change and prints typed absences instead of edges', () => {
    const layout = layoutEnvelopes(INBOX, INBOX.pull_requests, buildUmbrellas(INBOX), VIEWPORT);
    const marks = layout.envelopes.flatMap((envelope) => envelope.marks);
    expect(marks.map((mark) => [mark.row.pull_request.pull_request_id, mark.change, Math.round(mark.radius ?? -1), mark.absences, mark.beacons, mark.unevaluated])).toEqual([
      ['42', 155, 15, [], ['ci_failure', 'unresolved_review'], 1],
      ['43', 14, 10, ['head unobserved'], [], 0],
      ['8', 155, 15, [], [], 0],
    ]);
    expect(layout.links.map(({ link }) => `${link.code}:${link.from}->${link.to}`)).toEqual([
      'WORK:project.alpha:github:42->project.beta:github:8',
      'AGENT:project.alpha:github:43->project.beta:github:8',
    ]);
  });

  it('draws branch-only PRs hollow with no link at all', () => {
    const layout = layoutEnvelopes(INBOX_BRANCH_ONLY, INBOX_BRANCH_ONLY.pull_requests, buildUmbrellas(INBOX_BRANCH_ONLY), VIEWPORT);
    expect(layout.links).toEqual([]);
    expect(layout.envelopes.flatMap((envelope) => envelope.marks.map((mark) => [mark.hollow, mark.absences]))).toEqual([
      [true, ['not joined']],
      [true, ['not joined', 'head unobserved']],
      [true, ['not joined']],
    ]);
  });

  it('keeps a repository with no drawable PR as an envelope carrying the provider reason', () => {
    const inbox = {
      ...INBOX,
      projects: INBOX.projects.map((project) =>
        project.project_id === 'project.beta' ? { ...project, provider_state: 'not_published' as const } : project,
      ),
      omitted_projects: 2,
    };
    const rows = inbox.pull_requests.filter((row) => row.project_id === 'project.alpha');
    const layout = layoutEnvelopes(inbox, rows, buildUmbrellas(inbox), VIEWPORT);
    expect(layout.envelopes.map((envelope) => envelope.absence)).toEqual([null, 'not_published · requires github_read_authority.']);
    expect(layout.omitted).toEqual({ x: 16, y: 164, width: 335, height: 120, count: 2 });
  });
});
