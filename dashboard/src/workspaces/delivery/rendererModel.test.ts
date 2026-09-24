import { describe, expect, it } from 'vitest';
import { INBOX, INBOX_BRANCH_ONLY, T0 } from '../../test/deliveryFixtures.ts';
import {
  attentionCode,
  evidenceLinks,
  headJoin,
  headJoinSentence,
  observationWindow,
  uncorrelatedRows,
} from './rendererModel.ts';
import { buildUmbrellas } from './umbrella.ts';

const [PR42, PR43, PR8] = INBOX.pull_requests as [
  (typeof INBOX.pull_requests)[number],
  (typeof INBOX.pull_requests)[number],
  (typeof INBOX.pull_requests)[number],
];
const HOUR = 3_600_000_000;

describe('renderer readings', () => {
  it('prints a short engraved code per attention source', () => {
    expect(attentionCode('ci_failure')).toBe('CI');
    expect(attentionCode('unresolved_review')).toBe('REVIEW');
    expect(attentionCode('stale_provider_state')).toBe('STALE');
  });

  it('types the provider/indexed head join instead of assuming it', () => {
    expect(headJoin(PR42)).toEqual({ kind: 'joined', head: 'a'.repeat(40) });
    expect(headJoinSentence(headJoin(PR43))).toBe('provider head not observed · no read snapshot served');
    const moved = {
      ...PR8,
      indexed_head_commit_id: 'f'.repeat(40),
    };
    expect(headJoinSentence(headJoin(moved))).toBe('provider head bbbbbbb ≠ indexed head fffffff');
  });

  it('spans only daemon observation time and has no window without one', () => {
    expect(observationWindow(PR42)).toEqual({ start: T0 + 6 * HOUR, end: T0 + 7 * HOUR });
    expect(observationWindow(PR43)).toBeNull();
  });

  it('chains PRs that share a served identity without inventing a hub node', () => {
    const links = evidenceLinks(buildUmbrellas(INBOX), new Set(INBOX.pull_requests.map((row) => row.id)));
    expect(links.map((link) => [link.from, link.to, link.code, link.grade, link.identity])).toEqual([
      ['project.alpha:github:42', 'project.beta:github:8', 'WORK', 'explicit', 'work.retry-backoff'],
      ['project.alpha:github:43', 'project.beta:github:8', 'AGENT', 'inferred', 'agent.claude-code'],
    ]);
  });

  it('marks every row uncorrelated when only branch references are served', () => {
    expect([...uncorrelatedRows(buildUmbrellas(INBOX), INBOX.pull_requests)]).toEqual([]);
    expect([...uncorrelatedRows(buildUmbrellas(INBOX_BRANCH_ONLY), INBOX_BRANCH_ONLY.pull_requests)]).toEqual([
      'project.alpha:github:42',
      'project.alpha:github:43',
      'project.beta:github:8',
    ]);
  });
});
