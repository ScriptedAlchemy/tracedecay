import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { AnalyticsSubagentTreePayloadV1Schema } from '../../contracts/generated.ts';
import type { AnalyticsSubagentTreePayloadV1 } from '../../contracts/generated.ts';
import { SubagentTree } from './SubagentTree.tsx';

/**
 * The subagent tree surface.
 *
 * Every fixture below is parsed through the GENERATED contract before it is
 * rendered, so no test here can pass against a payload the daemon could not
 * send. That matters more than usual for this surface: the whole point of the
 * route is that the tree comes assembled from the store, so a hand-shaped
 * reading would be testing this file's imagination.
 */
function parsed(payload: unknown): AnalyticsSubagentTreePayloadV1 {
  return AnalyticsSubagentTreePayloadV1Schema.parse(payload);
}

function baseNode(overrides: Record<string, unknown>) {
  return {
    provider: 'codex',
    session_id: 'session.a',
    parent_session_id: null,
    agent: null,
    title: null,
    started_at: null,
    ended_at: null,
    is_subagent: false,
    parent_tool_use_id: null,
    depth: 0,
    descendants: 0,
    link: 'root',
    ...overrides,
  };
}

function tree(nodes: Record<string, unknown>[], overrides: Record<string, unknown> = {}) {
  return parsed({
    available: true,
    source: 'sessions',
    error: null,
    nodes,
    sessions_read: nodes.length,
    root_count: nodes.filter((entry) => entry.link === 'root').length,
    edge_count: nodes.filter((entry) => entry.link === 'linked').length,
    max_depth: nodes.reduce((deepest, entry) => Math.max(deepest, Number(entry.depth)), 0),
    missing_parent_count: nodes.filter((entry) => entry.link === 'missing_parent').length,
    cycle_count: nodes.filter((entry) => entry.link === 'cycle').length,
    truncated: false,
    ...overrides,
  });
}

const NESTED = [
  baseNode({
    session_id: 'session.root',
    agent: 'Codex',
    depth: 0,
    descendants: 2,
    started_at: 1_760_000_000,
    ended_at: 1_760_003_600,
  }),
  baseNode({
    session_id: 'session.child',
    parent_session_id: 'session.root',
    agent: 'Claude',
    depth: 1,
    descendants: 1,
    link: 'linked',
    is_subagent: true,
    parent_tool_use_id: 'toolu_01',
  }),
  baseNode({
    session_id: 'session.grandchild',
    parent_session_id: 'session.child',
    depth: 2,
    link: 'linked',
    is_subagent: true,
  }),
];

describe('SubagentTree', () => {
  it('draws parent/child edges as a nested tree, not a flat rollup', () => {
    render(<SubagentTree payload={tree(NESTED)} />);

    // A tree is one group here, not three islands — which is the whole
    // difference from the per-agent session rollup beside it.
    const root = document.querySelector('[data-subagent-tree-groups]');
    expect(root?.getAttribute('data-subagent-tree-groups')).toBe('1');

    const depths = [...document.querySelectorAll('[data-subagent-node]')].map((element) => [
      element.getAttribute('data-subagent-node'),
      element.getAttribute('data-subagent-depth'),
    ]);
    expect(depths).toEqual([
      ['session.root', '0'],
      ['session.child', '1'],
      ['session.grandchild', '2'],
    ]);
  });

  it('states the edge count and the reach rather than leaving them to the drawing', () => {
    render(<SubagentTree payload={tree(NESTED)} />);

    expect(screen.getByText(/delegation/i).textContent).toMatch(/2\s*delegation edges/i);
    expect(screen.getByText(/delegation/i).textContent).toMatch(/nested/i);
  });

  it('attributes a delegation to the tool call that made it', () => {
    render(<SubagentTree payload={tree(NESTED)} />);

    expect(screen.getByText(/delegated by tool call toolu_01/i)).toBeTruthy();
  });

  it('keeps a session with no parent apart from one whose parent was never ingested', () => {
    render(
      <SubagentTree
        payload={tree([
          baseNode({ session_id: 'session.real-root' }),
          baseNode({
            session_id: 'session.orphan',
            parent_session_id: 'session.missing',
            link: 'missing_parent',
          }),
        ])}
      />,
    );

    const orphan = document.querySelector('[data-subagent-link="missing_parent"]');
    expect(orphan?.textContent).toMatch(/not in this reading/i);
    expect(orphan?.textContent).toMatch(/cut edge and not a root/i);
    // The real root carries no such note.
    expect(document.querySelectorAll('[data-subagent-link="missing_parent"]').length).toBe(1);
  });

  it('surfaces a cycled session instead of dropping it from the count', () => {
    render(
      <SubagentTree
        payload={tree([
          baseNode({ session_id: 'session.a', parent_session_id: 'session.b', link: 'cycle' }),
          baseNode({ session_id: 'session.b', parent_session_id: 'session.a', link: 'cycle' }),
        ])}
      />,
    );

    expect(document.querySelectorAll('[data-subagent-node]').length).toBe(2);
    expect(document.querySelector('[data-subagent-link="cycle"]')?.textContent).toMatch(
      /closes on itself/i,
    );
    expect(document.querySelector('[data-subagent-tree-caveats]')?.textContent).toMatch(
      /reachable from no root/i,
    );
  });

  it('calls an edgeless reading a measured absence of delegation, not an unread tree', () => {
    render(
      <SubagentTree
        payload={tree([baseNode({ session_id: 'session.a' }), baseNode({ session_id: 'session.b' })])}
      />,
    );

    expect(screen.getByText(/not one parent\/child edge/i)).toBeTruthy();
    expect(screen.getByText(/measured absence of delegation, not an unread tree/i)).toBeTruthy();
  });

  it('calls an empty store empty rather than reporting no delegation', () => {
    render(<SubagentTree payload={tree([])} />);

    expect(screen.getByText(/empty store, not a project whose agents delegated nothing/i)).toBeTruthy();
    expect(document.querySelector('[data-subagent-tree]')?.getAttribute('data-subagent-tree')).toBe(
      'empty',
    );
  });

  it('says a ceiling read describes a prefix of the store', () => {
    render(
      <SubagentTree
        payload={tree([baseNode({ session_id: 'session.a' })], {
          sessions_read: 2_000,
          truncated: true,
        })}
      />,
    );

    expect(document.querySelector('[data-subagent-tree-truncated="true"]')?.textContent).toMatch(
      /prefix of the store/i,
    );
  });

  it('reads the store\'s second stamps as seconds', () => {
    render(<SubagentTree payload={tree(NESTED)} />);

    // 1_760_003_600 - 1_760_000_000 = 3600 seconds. Read as micros this would
    // be a sub-millisecond session; read as millis, 3.6 seconds.
    expect(document.querySelector('[data-subagent-node="session.root"]')?.textContent).toMatch(
      /3,600s/,
    );
    // The child records no end, and says so instead of drawing zero.
    expect(document.querySelector('[data-subagent-node="session.child"]')?.textContent).toMatch(
      /span unrecorded/i,
    );
  });
});
