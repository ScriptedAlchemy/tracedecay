import { render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { WorkGraphReadV1Schema } from '../../contracts/generated.ts';
import { workGraphRead } from '../../test/workGraphFixture.ts';
import type { WorkResult } from '../work/workApi.ts';
import type { WorkGraphReadV1 } from '../../contracts/index.ts';
import { AgentHandoffs } from './AgentHandoffs.tsx';
import { readHandoffFrontier } from './handoff.ts';

/**
 * The handoff frontier surface.
 *
 * Everything below drives the component through `readHandoffFrontier` over a
 * fixture the generated contract has already accepted, rather than through a
 * hand-shaped reading — so a test cannot pass against a frontier the daemon
 * could never send.
 */

const AT = 1_800_000_000_000_000;

function landed(read: unknown): WorkResult<WorkGraphReadV1> {
  // Parsed with the same schema `callWork` parses the wire with. A fixture the
  // contract rejects fails here instead of rendering a shape the daemon cannot
  // produce.
  return { outcome: 'value', value: WorkGraphReadV1Schema.parse(read) };
}

function frontierFixture() {
  return workGraphRead({
    tasks: [
      {
        taskId: 'task.alpha',
        handoffs: [
          {
            handoffId: 'handoff.alpha.1',
            fromActor: 'actor.planner',
            toActor: 'actor.builder',
            handedOffAt: AT - 7_200_000_000,
            evidenceFrontier: ['evidence.route-mounted'],
            unknowns: ['whether the daemon stamps an actor identity'],
          },
          {
            handoffId: 'handoff.alpha.2',
            fromActor: 'actor.builder',
            toActor: 'actor.review',
            handedOffAt: AT - 3_600_000_000,
            evidenceFrontier: ['evidence.table-accessible', 'evidence.tests-green'],
            unknowns: [],
          },
        ],
      },
      { taskId: 'task.beta' },
    ],
    observedAt: AT,
  });
}

describe('AgentHandoffs', () => {
  it('renders every handoff with the actors, the evidence and the unknowns it carried', () => {
    render(<AgentHandoffs reading={readHandoffFrontier(landed(frontierFixture()))} />);

    const table = screen.getByRole('table');
    // Both handoffs, and the actors on either end of each.
    expect(within(table).getByText('actor.planner')).toBeTruthy();
    expect(within(table).getAllByText('actor.builder').length).toBe(2);
    expect(within(table).getByText('actor.review')).toBeTruthy();

    // The evidence AND the unknowns. A row that printed only the evidence would
    // show a handoff carrying an open question as finished work.
    expect(
      within(table).getByText('whether the daemon stamps an actor identity'),
    ).toBeTruthy();
    expect(within(table).getByText('evidence.tests-green')).toBeTruthy();
    // The handoff that declared none says so rather than leaving a blank cell.
    expect(within(table).getByText('none declared')).toBeTruthy();
  });

  it('states the population the frontier is a subset of', () => {
    render(<AgentHandoffs reading={readHandoffFrontier(landed(frontierFixture()))} />);
    // Two handoffs over ONE of the two tasks: a frontier reported without its
    // denominator reads as the whole graph.
    expect(screen.getByText(/2 handoffs · 1 of 2 tasks · graph version 4/)).toBeTruthy();
    expect(screen.getByText(/3 evidence references were carried/)).toBeTruthy();
    expect(screen.getByText(/1 declared unknown/)).toBeTruthy();
  });

  it('carries the actor rollup as text and not only as a rail', () => {
    render(<AgentHandoffs reading={readHandoffFrontier(landed(frontierFixture()))} />);
    // The meter rails are aria-hidden by construction, so each actor's figures
    // have to be readable as characters. `builder` handed one on and received
    // one; `planner` handed one on and received none.
    const rollup = screen.getByText(/actors on the frontier/).parentElement!;
    expect(within(rollup).getByText('1↦1')).toBeTruthy();
    expect(within(rollup).getAllByText('1↦0').length).toBeGreaterThan(0);
  });

  it('never draws an unread frontier as an empty one', () => {
    render(
      <AgentHandoffs
        reading={readHandoffFrontier({
          outcome: 'refused',
          state: 'offline',
          detail: 'the daemon could not be reached',
        })}
      />,
    );

    expect(screen.getByText('Offline')).toBeTruthy();
    expect(screen.getByText(/the daemon could not be reached/)).toBeTruthy();
    expect(screen.getByText(/there is no frontier to be empty/)).toBeTruthy();
    // No count of any kind, and above all no zero.
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.queryByText(/no handoff on graph version/)).toBeNull();
    expect(document.querySelector('[data-agent-handoffs="refused"]')).toBeTruthy();
  });

  it('reports a pending read as pending rather than as no handoffs', () => {
    render(<AgentHandoffs reading={readHandoffFrontier(undefined)} />);
    expect(screen.getByText('Loading')).toBeTruthy();
    expect(screen.queryByText(/no handoff/)).toBeNull();
  });

  it('separates a graph that answered with no handoff from a graph that was not read', () => {
    render(
      <AgentHandoffs
        reading={readHandoffFrontier(
          landed(workGraphRead({ tasks: [{ taskId: 'task.alpha' }], observedAt: AT })),
        )}
      />,
    );

    expect(screen.getByText(/no handoff on graph version 4/)).toBeTruthy();
    expect(
      screen.getByText(/This is the graph saying nothing was handed between actors/),
    ).toBeTruthy();
    // A measured emptiness renders no failure chip: this read landed.
    expect(screen.queryByText('Offline')).toBeNull();
    expect(document.querySelector('[data-agent-handoffs-empty="true"]')).toBeTruthy();
  });

  it('says where the frontier came from, and that the token operations are not it', () => {
    render(<AgentHandoffs reading={readHandoffFrontier(landed(frontierFixture()))} />);
    expect(screen.getByText(/work\.views/)).toBeTruthy();
    expect(screen.getByText(/cannot enumerate a frontier/)).toBeTruthy();
  });
});
