import { describe, expect, it } from 'vitest';
import { INBOX } from '../../test/deliveryFixtures.ts';
import { readDeliveryLocation } from './deliveryLocation.ts';
import { edgesFor, filterInbox } from './inboxFilter.ts';

function ids(query: string): string[] {
  return filterInbox(INBOX, readDeliveryLocation(new URLSearchParams(query))).map((row) => row.id);
}

describe('filterInbox', () => {
  it('admits every row with no filter and keeps the daemon order', () => {
    expect(ids('')).toEqual([
      'project.alpha:github:42',
      'project.alpha:github:43',
      'project.beta:github:8',
    ]);
  });

  it('narrows by project, status, draft, provider state and attention source', () => {
    expect(ids('project=project.beta')).toEqual(['project.beta:github:8']);
    expect(ids('status=open')).toHaveLength(3);
    expect(ids('status=merged')).toEqual([]);
    expect(ids('status=draft')).toEqual(['project.alpha:github:43']);
    expect(ids('provider=stale')).toEqual(['project.beta:github:8']);
    expect(ids('attention=ci_failure')).toEqual(['project.alpha:github:42']);
  });

  it('treats unresolved-only as any active attention item', () => {
    expect(ids('unresolved=1')).toEqual(['project.alpha:github:42']);
  });

  it('excludes a row whose identity was not served when a status is requested', () => {
    const withoutIdentity = {
      ...INBOX,
      pull_requests: INBOX.pull_requests.map((row) => ({
        ...row,
        pull_request: { ...row.pull_request, identity: null },
      })),
    };
    expect(
      filterInbox(withoutIdentity, readDeliveryLocation(new URLSearchParams('status=open'))),
    ).toEqual([]);
  });
});

describe('edgesFor', () => {
  it('returns only the edges joined to that project and pull request', () => {
    const edges = edgesFor(INBOX, INBOX.pull_requests[0]!);
    expect(edges.map((edge) => edge.basis.kind)).toEqual([
      'branch_pull_request_reference',
      'shared_work_objective',
      'session_git_relation',
    ]);
  });
});
