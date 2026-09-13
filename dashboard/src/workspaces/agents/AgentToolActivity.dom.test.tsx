import { render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { AgentToolActivity } from './AgentToolActivity.tsx';
import type { ToolActivityRead } from './activity.ts';

/**
 * Tool activity.
 *
 * The three readings this surface must keep apart are a served figure, a figure
 * that cannot be computed from what was served, and a source that answered
 * carrying nothing. Every case below is one of those three.
 */

function read(overrides: Partial<ToolActivityRead> = {}): ToolActivityRead {
  return {
    tool_call_count: 1_000,
    mcp_tool_call_count: 750,
    tracedecay_call_count: 640,
    by_tool_category: [
      { tool_category: 'mcp', count: 750 },
      { tool_category: 'shell', count: 180 },
      { tool_category: 'edit', count: 70 },
    ],
    by_tool: [
      { tool_name: 'tracedecay_grep', count: 400 },
      { tool_name: 'Bash', count: 180 },
    ],
    ratios: { tool_calls_per_message: 0.4 },
    recent_hooks: [
      { agent: 'Codex', tool_name: 'tracedecay_grep', session_id: 's1' },
      { agent: 'Codex', tool_name: 'tracedecay_grep', session_id: 's1' },
      { agent: 'Codex', tool_name: 'Bash', session_id: 's2' },
      { agent: 'Claude', tool_name: 'tracedecay_body', session_id: 's3' },
    ],
    hook_window: { truncated: true },
    ...overrides,
  };
}

describe('AgentToolActivity', () => {
  it('divides the window into MCP and non-MCP calls against a served total', () => {
    render(<AgentToolActivity payload={read()} />);

    // The headline sentence is broken across `td-value` spans.
    const headline = document.querySelector('[data-agent-tool-activity="read"] > p')!;
    expect(headline.textContent).toContain('1,000 tool calls in the window');
    expect(headline.textContent).toContain('0.40 per message');
    const split = document.querySelector('[data-agent-tool-split="drawn"]')!;
    expect(within(split as HTMLElement).getByText('750')).toBeTruthy();
    expect(within(split as HTMLElement).getByText('250')).toBeTruthy();
    expect(within(split as HTMLElement).getByText(/640 of them were calls into/)).toBeTruthy();
  });

  it('refuses to subtract a remainder when only one of the two totals was served', () => {
    render(<AgentToolActivity payload={read({ tool_call_count: undefined })} />);

    expect(document.querySelector('[data-agent-tool-split="unsplit"]')).toBeTruthy();
    expect(document.querySelector('[data-agent-tool-split="drawn"]')).toBeNull();
    expect(screen.getByText(/the fold served no tool total/)).toBeTruthy();
    // The MCP count it DID serve is still reported, as a floor and not a share.
    expect(screen.getByText(/750 MCP tool calls, which is a floor and not a share/)).toBeTruthy();
    // And nothing invents a zero for the missing half.
    expect(screen.queryByText('not through MCP')).toBeNull();
  });

  it('reports a store that disagrees with itself rather than clamping the remainder', () => {
    render(<AgentToolActivity payload={read({ tool_call_count: 100, mcp_tool_call_count: 750 })} />);

    expect(document.querySelector('[data-agent-tool-split="contradiction"]')).toBeTruthy();
    expect(screen.getByText(/The two disagree, so no remainder is drawn/)).toBeTruthy();
    expect(document.querySelector('[data-agent-tool-split="drawn"]')).toBeNull();
  });

  it('attributes tool calls to the agents the hook tape names', () => {
    render(<AgentToolActivity payload={read()} />);

    const list = document.querySelector('[data-agent-attribution]')!;
    expect(list.getAttribute('data-agent-attribution')).toBe('2');
    const codex = document.querySelector('[data-agent-attribution-agent="Codex"]')! as HTMLElement;
    // Three rows on the tape, across two sessions, and the tools it reached for
    // are readable as text rather than only encoded in a rail.
    expect(within(codex).getByText('3')).toBeTruthy();
    expect(within(codex).getByText(/2 sessions · tracedecay_grep 2 · Bash 1/)).toBeTruthy();
    // A suffix is not a total, and the surface says which it is.
    expect(screen.getByText(/a recent suffix/)).toBeTruthy();
    expect(screen.getByText(/floors, not totals/)).toBeTruthy();
  });

  it('keeps a tape that named no agent from reading as agents calling nothing', () => {
    render(<AgentToolActivity payload={read({ recent_hooks: [] })} />);

    expect(document.querySelector('[data-agent-attribution="none"]')).toBeTruthy();
    expect(
      screen.getByText(/not a measurement that agents called nothing/),
    ).toBeTruthy();
    expect(screen.getByText(/It served no hook rows at all\./)).toBeTruthy();
    expect(document.querySelector('[data-agent-attribution-agent="Codex"]')).toBeNull();
  });

  it('excludes unattributed hook rows from the ranking and states how many', () => {
    render(
      <AgentToolActivity
        payload={read({
          recent_hooks: [
            { agent: 'Codex', tool_name: 'Bash', session_id: 's1' },
            { agent: '', tool_name: 'Bash', session_id: 's2' },
            { tool_name: 'Bash', session_id: 's3' },
          ],
        })}
      />,
    );

    expect(document.querySelector('[data-agent-attribution]')!.getAttribute('data-agent-attribution')).toBe('1');
    expect(
      screen.getByText(/2 further hook rows name no agent/),
    ).toBeTruthy();
  });
});
