import { describe, expect, it } from 'vitest';
import { INBOX, INBOX_BRANCH_ONLY } from '../../test/deliveryFixtures.ts';
import { membershipGrade } from './evidence.ts';
import { buildUmbrellas, relatedAcrossProjects, umbrellasFor } from './umbrella.ts';

describe('buildUmbrellas', () => {
  it('groups pull requests only under a shared correlating identity', () => {
    const projection = buildUmbrellas(INBOX);
    expect(projection.authority).toEqual({ state: 'served', edges: 5 });
    expect(projection.umbrellas.map((umbrella) => umbrella.id)).toEqual([
      'shared_work_objective:work.retry-backoff',
      'shared_agent:agent.claude-code',
    ]);
    const objective = projection.umbrellas[0]!;
    expect(objective.members.map((member) => member.id)).toEqual([
      'project.alpha:github:42',
      'project.beta:github:8',
    ]);
    expect(objective.projectIds).toEqual(['project.alpha', 'project.beta']);
    expect(objective.grade).toBe('explicit');
    expect(objective.activeAttention).toBe(2);
  });

  it('never groups by the branch reference or by proximity', () => {
    const projection = buildUmbrellas(INBOX_BRANCH_ONLY);
    expect(projection.umbrellas).toEqual([]);
    expect(projection.authority.state).toBe('unavailable');
    if (projection.authority.state === 'unavailable') {
      expect(projection.authority.reason).toMatch(/no cross-PR correlation edge/);
    }
  });

  it('drops a single-member identity rather than inventing an outcome', () => {
    const projection = buildUmbrellas(INBOX);
    // session.alpha.1 is joined to #42 only.
    expect(
      projection.umbrellas.some((umbrella) => umbrella.basisKind === 'session_git_relation'),
    ).toBe(false);
  });

  it('keeps grades on the ladder and never upgrades a correlation', () => {
    for (const edge of INBOX.membership_edges) {
      const grade = membershipGrade(edge.basis);
      if (edge.basis.kind === 'branch_pull_request_reference') expect(grade).toBe('exact');
      if (edge.basis.kind === 'session_git_relation') expect(grade).toBe('inferred');
      if (edge.basis.kind === 'shared_agent') expect(grade).toBe('inferred');
      if (edge.basis.kind === 'shared_work_objective') expect(grade).toBe('explicit');
    }
    const projection = buildUmbrellas(INBOX);
    expect(projection.umbrellas.every((umbrella) => umbrella.grade !== 'exact')).toBe(true);
  });

  it('counts a correlating edge whose pull request was not admitted', () => {
    const projection = buildUmbrellas({
      ...INBOX,
      membership_edges: [
        ...INBOX.membership_edges,
        {
          id: 'project.gamma:99:shared_work_objective:0',
          project_id: 'project.gamma',
          pull_request_id: '99',
          basis: { kind: 'shared_work_objective', work_item_id: 'work.retry-backoff' },
        },
      ],
    });
    expect(projection.unresolvedEdges).toBe(1);
  });

  it('is deterministic across input order', () => {
    const shuffled = {
      ...INBOX,
      membership_edges: [...INBOX.membership_edges].reverse(),
      pull_requests: [...INBOX.pull_requests].reverse(),
    };
    expect(buildUmbrellas(shuffled)).toEqual(buildUmbrellas(INBOX));
  });

  it('resolves membership and cross-project related rows from the same edges', () => {
    const projection = buildUmbrellas(INBOX);
    expect(umbrellasFor(projection, 'project.alpha:github:42').map((u) => u.id)).toEqual([
      'shared_work_objective:work.retry-backoff',
    ]);
    const related = relatedAcrossProjects(projection, 'project.alpha');
    expect(related.map((entry) => entry.member.id)).toEqual(['project.beta:github:8']);
    expect(relatedAcrossProjects(projection, 'project.gamma')).toEqual([]);
  });
});
