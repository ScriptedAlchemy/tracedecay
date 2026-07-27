import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { AutomationsPage } from './AutomationsPage.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

/**
 * The two awaiting-review counts gate human approval of agent-proposed facts
 * and skill drafts, so "nobody could read the queue" and "the queue is empty"
 * must never render the same way. The Playwright axe gate proves this on a real
 * browser; these run in `npm test`, which is what CI executes on every change.
 *
 * Every count assertion here goes through `reviewTile`, which reads the value
 * out of the tile carrying a given label — the same way the browser gate's
 * `reviewTiles` does. Asserting that "5" is somewhere on the page would pass
 * just as happily with the two queues swapped.
 */
describe('AutomationsPage review queues', () => {
  it('puts each measured count on its own tile, marked as measured', async () => {
    stubAutomation({ status: scheduler(measured(5), measured(2)) });
    renderAutomations();
    await screen.findByText('pending proposals');

    expect(reviewTile('pending proposals')).toEqual({ value: '5', evidence: 'measured' });
    expect(reviewTile('pending skills')).toEqual({ value: '2', evidence: 'measured' });
    expect(screen.queryByText(/Awaiting-review counts are unknown/i)).toBeNull();
  });

  it('prints zero only for a queue that was read and found empty', async () => {
    stubAutomation({ status: scheduler(measured(0), measured(0)) });
    renderAutomations();
    await screen.findByText('pending proposals');

    // A zero is a legitimate reading. The defect is a zero standing in for a
    // reading nobody took, so the evidence class has to come with it.
    expect(reviewTile('pending proposals')).toEqual({ value: '0', evidence: 'measured' });
    expect(reviewTile('pending skills')).toEqual({ value: '0', evidence: 'measured' });
    expect(screen.queryByText(/Awaiting-review counts are unknown/i)).toBeNull();
  });

  it('never prints a number for a queue whose read failed, and repeats the daemon’s reason', async () => {
    stubAutomation({
      status: scheduler(
        unreadable('the project fact authority could not be read: database is locked'),
        unreadable('the managed skill store could not be read: permission denied'),
      ),
    });
    renderAutomations();
    await screen.findByText('pending proposals');

    expect(reviewTile('pending proposals')).toEqual({ value: '—', evidence: 'unknown' });
    expect(reviewTile('pending skills')).toEqual({ value: '—', evidence: 'unknown' });
    const banner = screen.getByText(/Awaiting-review counts are unknown, not zero/i).textContent;
    expect(banner).toContain('The fact proposals queue: the project fact authority could not be read: database is locked.');
    expect(banner).toContain('The skill drafts queue: the managed skill store could not be read: permission denied.');
  });

  it('ignores the flat pending_* mirrors when the review union says the queue was not read', async () => {
    // The mirrors exist on the wire and carry numbers here. `pending_review` is
    // the authority: a surface that fell back to a mirror would print 7 and 9
    // as though someone had counted them.
    stubAutomation({
      status: {
        ...scheduler(unreadable('the fact store is mid-migration'), unreadable('no skills root')),
        pending_fact_proposals: 7,
        pending_skills: 9,
      },
    });
    renderAutomations();
    await screen.findByText('pending proposals');

    expect(reviewTile('pending proposals')).toEqual({ value: '—', evidence: 'unknown' });
    expect(reviewTile('pending skills')).toEqual({ value: '—', evidence: 'unknown' });
    expect(screen.queryByText('7')).toBeNull();
    expect(screen.queryByText('9')).toBeNull();
  });

  it('keeps one queue readable when the other is not, and names only the queue that failed', async () => {
    stubAutomation({
      status: scheduler(
        measured(3),
        unreadable('the user profile root could not be resolved: no home directory'),
      ),
    });
    renderAutomations();
    await screen.findByText('pending proposals');

    expect(reviewTile('pending proposals')).toEqual({ value: '3', evidence: 'measured' });
    expect(reviewTile('pending skills')).toEqual({ value: '—', evidence: 'unknown' });
    const banner = screen.getByText(/Awaiting-review counts are unknown, not zero/i).textContent;
    expect(banner).toContain('The skill drafts queue: the user profile root could not be resolved');
    expect(banner).not.toContain('The fact proposals queue');
  });

  it('refuses to mine counts out of a payload that fails the generated contract', async () => {
    // The flat `pending_*` mirrors without the discriminated union. The bundle
    // ships inside the binary that answers this route, so this is not a version
    // skew to paper over — it is a payload the contract does not admit, and the
    // honest answer is the unsupported-schema state rather than a partial panel.
    stubAutomation({
      status: {
        status: 'configured',
        paused: false,
        enabled: true,
        scheduler_tick_secs: 900,
        pending_fact_proposals: null,
        pending_skills: null,
        now: Math.floor(Date.now() / 1000),
        last_session_activity: null,
        project_config_path: '/x/automation.toml',
        control_path: '/x/automation.control.json',
        tasks: [],
      },
    });
    renderAutomations();

    expect(
      await screen.findByText(/The daemon answered with a shape this build does not understand/i),
    ).toBeTruthy();
    // Not "the tiles read unknown" — no scheduler tile is rendered at all, so
    // there is nothing on screen for a reader to mistake for a reading.
    expect(tileLabels()).not.toContain('pending proposals');
    expect(tileLabels()).not.toContain('pending skills');
    expect(screen.queryByText(/Awaiting-review counts are unknown/i)).toBeNull();
  });
});

/**
 * Pause and resume are the page's only writes, and the rule they exist to
 * honor is that the control never paints a state it has not observed.
 */
describe('AutomationsPage scheduler control', () => {
  it('shows the state the server re-read after the change, not the one that was clicked', async () => {
    let paused = false;
    const fetchMock = stubAutomation({
      status: () => jsonResponse(scheduler(measured(5), measured(2), { paused })),
      pause: () => {
        paused = true;
        // The server's re-read disagrees with the pre-click screen on more than
        // the flag: a proposal was approved in between. Both have to land, which
        // they only can if the surface is rendering the response body.
        return jsonResponse(scheduler(measured(4), measured(2), { paused }));
      },
    });
    renderAutomations();

    await userEvent.click(await screen.findByRole('button', { name: 'Pause scheduler' }));

    expect(await screen.findByRole('button', { name: 'Resume scheduler' })).toBeTruthy();
    expect(reviewTile('pending proposals').value).toBe('4');
    const call = fetchMock.mock.calls.find(([url]) => String(url).endsWith('/scheduler/pause'));
    expect(call?.[1]?.method).toBe('POST');
  });

  it('reports a control whose reply could not be read, and leaves the last real reading up', async () => {
    stubAutomation({
      status: () => jsonResponse(scheduler(measured(5), measured(2))),
      // A proxy's HTML error page: the POST may well have taken effect, and
      // this dashboard cannot tell. Guessing either way would be the falsehood.
      pause: () =>
        new Response('<html>502 Bad Gateway</html>', {
          status: 200,
          headers: { 'content-type': 'text/html' },
        }),
    });
    renderAutomations();

    await userEvent.click(await screen.findByRole('button', { name: 'Pause scheduler' }));

    expect(
      await screen.findByText(/whether the scheduler changed is unknown/i),
    ).toBeTruthy();
    // The toggle did not flip on an unreadable answer, and the counts on screen
    // are still the ones the last successful read produced.
    expect(screen.getByRole('button', { name: 'Pause scheduler' })).toBeTruthy();
    expect(reviewTile('pending proposals')).toEqual({ value: '5', evidence: 'measured' });
  });
});

/**
 * One review tile, read the way the browser gate reads it: the value and the
 * evidence marker that belong to a specific label. Throws rather than returning
 * a blank when the tile is missing, so a vanished tile fails as a vanished tile
 * instead of as a value mismatch.
 */
function reviewTile(label: string): { value: string; evidence: string } {
  const legend = screen.queryByText(label, { selector: '.td-legend' });
  if (legend === null) {
    throw new Error(`no tile labelled "${label}" is on the page (found: ${tileLabels().join(', ')})`);
  }
  const numeric = legend.parentElement?.querySelector('[data-cell="numeric"]');
  if (!numeric) throw new Error(`the "${label}" tile rendered no value cell`);
  return {
    value: (numeric.textContent ?? '').trim(),
    // The evidence marker is the readout's annotation slot, which follows the
    // value. A tile that lost its marker must not read as an unmarked number.
    evidence: (numeric.parentElement?.nextElementSibling?.textContent ?? '').trim(),
  };
}

/** Every tile label currently rendered, for absence assertions. */
function tileLabels(): string[] {
  return Array.from(document.querySelectorAll('.td-legend')).map((el) =>
    (el.textContent ?? '').trim(),
  );
}

type Reading =
  | { state: 'measured'; count: number; reason: null }
  | { state: 'unreadable'; count: null; reason: string };

function measured(count: number): Reading {
  return { state: 'measured', count, reason: null };
}

function unreadable(reason: string): Reading {
  return { state: 'unreadable', count: null, reason };
}

/** The scheduler payload exactly as `automation_scheduler_api.rs` emits it. */
function scheduler(
  factProposals: Reading,
  skills: Reading,
  options: { paused?: boolean } = {},
): Record<string, unknown> {
  return {
    status: 'configured',
    paused: options.paused ?? false,
    enabled: true,
    scheduler_tick_secs: 900,
    pending_fact_proposals: factProposals.count,
    pending_skills: skills.count,
    pending_review: { fact_proposals: factProposals, skills },
    now: Math.floor(Date.now() / 1000),
    last_session_activity: Math.floor(Date.now() / 1000) - 1200,
    project_config_path: '/x/automation.toml',
    control_path: '/x/automation.control.json',
    tasks: [],
  };
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

/** A reply is a JSON body, or a function when the test needs the response to
 * change between calls or to be something JSON cannot express. */
type Reply = unknown | (() => Response);

/**
 * Routes by trailing path segment: `status`, `pause`, `resume`, `jobs`,
 * `skills`, `fact-proposals`. Anything the test does not name answers empty
 * rather than failing, so a queue assertion never trips over an unrelated
 * panel. Returns the mock so a test can assert what was actually requested.
 */
function stubAutomation(replies: Record<string, Reply>) {
  const fallbacks: Record<string, unknown> = {
    jobs: { jobs: [], count: 0 },
    skills: { skills: [] },
    'fact-proposals': { proposals: [] },
  };
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const endpoint = String(input).split('?')[0]?.split('/').pop() ?? '';
    void init;
    const reply = endpoint in replies ? replies[endpoint] : (fallbacks[endpoint] ?? {});
    return typeof reply === 'function' ? (reply as () => Response)() : jsonResponse(reply);
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

function renderAutomations() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  render(
    <QueryClientProvider client={client}>
      <AutomationsPage />
    </QueryClientProvider>,
  );
}
