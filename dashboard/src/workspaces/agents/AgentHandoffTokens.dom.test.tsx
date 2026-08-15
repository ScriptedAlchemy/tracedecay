import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import {
  AnalyticsSubagentTreePayloadV1Schema,
  ListTaskHandoffsResultV1Schema,
} from '../../contracts/generated.ts';
import { AgentHandoffTokens } from './AgentHandoffTokens.tsx';
import { newestTreeSession } from './handoffTokenQuery.ts';
import { readHandoffTokens } from './handoffTokens.ts';

function token(overrides: Record<string, unknown>) {
  return {
    token_digest: `sha256:${'a'.repeat(64)}`,
    issued_request_id: 'request.issue',
    session_id: 'lsp-session.task',
    kind: 'task',
    target: {
      kind: 'task',
      task_id: 'task.alpha',
      version: 9,
      owner_version_digest: `sha256:${'b'.repeat(64)}`,
    },
    issued_at: 1_000_000,
    expires_at: 61_000_000,
    state: 'open',
    consumed_at: null,
    ...overrides,
  };
}

function reading(handoffs: Record<string, unknown>[], sessionId = 'lsp-session.task') {
  const counted = (state: string) => handoffs.filter((entry) => entry.state === state).length;
  return readHandoffTokens(sessionId, {
    outcome: 'value',
    value: ListTaskHandoffsResultV1Schema.parse({
      observed_at: 2_000_000,
      handoffs,
      open_count: counted('open'),
      consumed_count: counted('consumed'),
      expired_count: counted('expired'),
      truncated: false,
    }),
  });
}

describe('AgentHandoffTokens', () => {
  it('gives a lapsed token its own section, because nothing else records it', () => {
    render(
      <AgentHandoffTokens
        reading={reading([
          token({ state: 'expired', token_digest: `sha256:${'1'.repeat(64)}` }),
          token({
            state: 'consumed',
            token_digest: `sha256:${'2'.repeat(64)}`,
            consumed_at: 1_500_000,
          }),
        ])}
      />,
    );

    expect(screen.getByRole('region', { name: /lapsed handoff tokens/i })).toBeTruthy();
    expect(screen.getByText(/only surface on which a dropped handoff is visible/i)).toBeTruthy();
    expect(
      document.querySelector('[data-handoff-token-state="expired"]'),
    ).toBeTruthy();
  });

  it('states that an empty frontier is about the reader, not about the tokens', () => {
    render(<AgentHandoffTokens reading={reading([])} />);

    expect(
      screen.getByText(/not evidence that no\s+handoff tokens exist/i),
    ).toBeTruthy();
  });

  it('keeps an unasked question apart from an empty frontier', () => {
    render(<AgentHandoffTokens reading={readHandoffTokens(null, undefined)} />);

    expect(document.querySelector('[data-handoff-tokens="unasked"]')).toBeTruthy();
    expect(screen.getByText(/not an empty frontier/i)).toBeTruthy();
  });

  it('renders a token by digest and never by bearer', () => {
    render(<AgentHandoffTokens reading={reading([token({})])} />);

    const row = document.querySelector('[data-handoff-token]');
    expect(row?.getAttribute('data-handoff-token')).toMatch(/^sha256:/);
    expect(screen.getByText(/task task\.alpha @ v9/)).toBeTruthy();
  });
});

describe('newestTreeSession', () => {
  const tree = (nodes: Record<string, unknown>[]) =>
    AnalyticsSubagentTreePayloadV1Schema.parse({
      available: true,
      source: 'sessions',
      error: null,
      nodes,
      sessions_read: nodes.length,
      root_count: nodes.length,
      edge_count: 0,
      max_depth: 0,
      missing_parent_count: 0,
      cycle_count: 0,
      truncated: false,
    });

  const node = (overrides: Record<string, unknown>) => ({
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
  });

  it('picks the newest tree top', () => {
    expect(
      newestTreeSession(
        tree([
          node({ session_id: 'older', started_at: 1_000 }),
          node({ session_id: 'newer', started_at: 2_000 }),
        ]),
      ),
    ).toBe('newer');
  });

  it('ignores nodes that are not tree tops', () => {
    expect(
      newestTreeSession(
        tree([
          node({ session_id: 'top', started_at: 1_000 }),
          node({ session_id: 'child', started_at: 9_000, depth: 1, link: 'linked' }),
        ]),
      ),
    ).toBe('top');
  });

  it('answers null when there is no session to ask about', () => {
    expect(newestTreeSession(null)).toBeNull();
    expect(newestTreeSession(tree([]))).toBeNull();
  });
});
