import { describe, expect, it } from 'vitest';
import { INBOX, INBOX_BRANCH_ONLY } from '../../test/deliveryFixtures.ts';
import { buildUmbrellas } from './umbrella.ts';
import { FIELD_HEIGHT, FIELD_WIDTH, arcPath, layoutField } from './umbrellaLayout.ts';

describe('layoutField', () => {
  it('places one hub per visible project and one node per admitted row', () => {
    const projection = buildUmbrellas(INBOX);
    const first = layoutField(INBOX, INBOX.pull_requests, projection, null);
    expect(first.hubs.map((hub) => hub.projectId)).toEqual(['project.alpha', 'project.beta']);
    expect(first.nodes.map((node) => node.id).sort()).toEqual(
      INBOX.pull_requests.map((row) => row.id).sort(),
    );
    for (const node of first.nodes) {
      expect(node.x).toBeGreaterThan(0);
      expect(node.x).toBeLessThan(FIELD_WIDTH);
      expect(node.y).toBeGreaterThan(0);
      expect(node.y).toBeLessThan(FIELD_HEIGHT);
    }
  });

  it('draws an umbrella root at the centroid of its members with one graded edge each', () => {
    const projection = buildUmbrellas(INBOX);
    const layout = layoutField(INBOX, INBOX.pull_requests, projection, null);
    expect(layout.roots.map((root) => root.umbrella.id)).toEqual([
      'shared_work_objective:work.retry-backoff',
      'shared_agent:agent.claude-code',
    ]);
    const objective = layout.roots[0]!;
    const members = layout.nodes.filter((node) =>
      objective.umbrella.members.some((member) => member.id === node.id),
    );
    const cx = members.reduce((sum, node) => sum + node.x, 0) / members.length;
    expect(objective.x).toBeCloseTo(cx, 6);
    const edges = layout.edges.filter((edge) => edge.umbrellaId === objective.umbrella.id);
    expect(edges).toHaveLength(2);
    expect(edges.every((edge) => edge.grade === 'explicit')).toBe(true);
  });

  it('focuses one umbrella at the centre and hides rows outside it', () => {
    const projection = buildUmbrellas(INBOX);
    const layout = layoutField(
      INBOX,
      INBOX.pull_requests,
      projection,
      'shared_agent:agent.claude-code',
    );
    expect(layout.roots).toHaveLength(1);
    expect(layout.roots[0]!.x).toBe(FIELD_WIDTH / 2);
    expect(layout.roots[0]!.y).toBe(FIELD_HEIGHT / 2);
    expect(layout.nodes.map((node) => node.id).sort()).toEqual([
      'project.alpha:github:43',
      'project.beta:github:8',
    ]);
  });

  it('draws no root and no edge when correlation is unavailable', () => {
    const projection = buildUmbrellas(INBOX_BRANCH_ONLY);
    const layout = layoutField(INBOX_BRANCH_ONLY, INBOX_BRANCH_ONLY.pull_requests, projection, null);
    expect(layout.roots).toEqual([]);
    expect(layout.edges).toEqual([]);
    expect(layout.nodes).toHaveLength(3);
  });

  it('bows an arc toward the centre and prints finite coordinates', () => {
    const path = arcPath({ x: 0, y: 0 }, { x: 100, y: 0 }, { x: 50, y: 100 });
    expect(path).toBe('M 0.0 0.0 Q 50.0 25.0 100.0 0.0');
  });
});
