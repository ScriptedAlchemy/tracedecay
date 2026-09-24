import { describe, expect, it } from 'vitest';
import { INBOX, OVERVIEW_ALPHA, OVERVIEW_LOCAL_ONLY } from '../../test/deliveryFixtures.ts';
import { edgesFor } from './inboxFilter.ts';
import { buildJourney } from './journey.ts';
import { buildTransit, gradeSummary } from './transit.ts';

const [PR42, PR43] = INBOX.pull_requests as [
  (typeof INBOX.pull_requests)[number],
  (typeof INBOX.pull_requests)[number],
];

function transit(row: typeof PR42, overview: typeof OVERVIEW_ALPHA | null) {
  const edges = edgesFor(INBOX, row);
  return buildTransit(row, edges, overview === null ? null : buildJourney(overview, { row, edges }));
}

describe('buildTransit', () => {
  it('reads the served PR as four graded stations joined by named bases', () => {
    const model = transit(PR42, OVERVIEW_ALPHA);
    expect(model.stations.map((station) => [station.id, station.state, station.grade, gradeSummary(station)])).toEqual([
      ['session', 'evidence', 'inferred', '1 EXPLICIT · 1 INFERRED'],
      ['code', 'evidence', 'exact', '4 EXACT'],
      ['verification', 'evidence', 'exact', '6 EXACT'],
      ['next', 'evidence', 'exact', '2 EXACT'],
    ]);
    expect(model.links).toEqual([
      { from: 'session', to: 'code', grade: 'inferred', basis: 'session–Git relation' },
      { from: 'code', to: 'verification', grade: 'exact', basis: 'provider head = indexed head' },
      { from: 'verification', to: 'next', grade: 'exact', basis: 'attention bound to named sources' },
    ]);
    const session = model.stations[0]!;
    expect(session.branches.map((branch) => [branch.kind, branch.identities])).toEqual([
      ['objective', ['work.retry-backoff']],
      ['session', ['session.alpha.1']],
      ['agent', []],
      ['handoff', []],
    ]);
    expect(model.stations[3]!.reasons).toEqual(['OVERLAP not evaluated · unavailable · unsupported coverage']);
  });

  it('prints the unserved provider lanes on CI / review instead of skipping it', () => {
    const verification = transit(PR42, OVERVIEW_LOCAL_ONLY).stations[2]!;
    expect(verification.reasons).toEqual([
      'Checks · not published · requires ci_provider_read_authority',
      'Reviews · not published · requires github_read_authority',
    ]);
    expect(verification.items.map((item) => item.label)).toEqual(['CI · CI failure', 'REVIEW · Unresolved review']);
  });

  it('keeps a PR with no joins, no reads and no attention as NO EVIDENCE / SERVED EMPTY bands', () => {
    const bare = { ...PR43, attention: [] };
    const model = buildTransit(bare, [], null);
    expect(model.stations.map((station) => [station.id, station.state])).toEqual([
      ['session', 'no_evidence'],
      ['code', 'evidence'],
      ['verification', 'no_evidence'],
      ['next', 'served_empty'],
    ]);
    expect(model.stations[0]!.reasons).toEqual([
      'No session–Git relation, agent attribution, handoff or Work objective is joined to this pull request.',
      'Agent reasoning is not reconstructed.',
    ]);
    expect(model.links.map((link) => [link.grade, link.basis])).toEqual([
      ['unavailable', 'no evidence to join'],
      ['unavailable', 'no evidence to join'],
      ['unavailable', 'no evidence to join'],
    ]);
    expect(model.span).toBeNull();
  });
});
