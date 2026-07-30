import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
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
 * A list body that is not this route's answer must not resolve to an empty
 * collection.
 *
 * The schemas these panels parse with used to make the collection optional and
 * offer an `items` alternative no handler has ever sent, so anything lacking
 * the real key fell through `?? []` and printed as a queue that had been read
 * and found empty. Requiring the field the handler always writes is what turns
 * those bodies into the unsupported-schema state instead.
 */
describe('AutomationsPage list contracts', () => {
  it('refuses a skills body with no skills key instead of reporting an empty store', async () => {
    stubAutomation({
      status: scheduler(measured(0), measured(0)),
      // What an unmounted profile root would leave behind: a well-formed JSON
      // object that says nothing about skills at all.
      skills: { error: 'managed skill root could not be resolved' },
    });
    renderAutomations();

    const panel = await settledPanel('Managed skills');
    expect(within(panel).queryByText(/no managed skills/i)).toBeNull();
    expect(panelState(panel)).toBe('unsupported_schema');
  });

  it('refuses a proposals body with no proposals key instead of reporting an empty queue', async () => {
    stubAutomation({
      status: scheduler(measured(0), measured(0)),
      'fact-proposals': { error: 'fact proposal authority unavailable' },
    });
    renderAutomations();

    const panel = await settledPanel('Fact proposals');
    expect(within(panel).queryByText(/no pending fact proposals/i)).toBeNull();
    expect(panelState(panel)).toBe('unsupported_schema');
  });

  it('does not accept the `items` shape, which no automation handler sends', async () => {
    // This key was the card's second fallback for years. Accepting it meant any
    // body carrying an empty `items` rendered as a read, empty queue.
    stubAutomation({
      status: scheduler(measured(0), measured(0)),
      skills: { items: [], count: 0 },
    });
    renderAutomations();

    const panel = await settledPanel('Managed skills');
    expect(within(panel).queryByText(/no managed skills/i)).toBeNull();
    expect(panelState(panel)).toBe('unsupported_schema');
  });

  it('shows a proposal that carries no fact request as carrying none, not as its id', async () => {
    stubAutomation({
      status: scheduler(measured(1), measured(0)),
      'fact-proposals': proposalsBody([proposal('fp-orphan', null)]),
    });
    renderAutomations();

    const panel = await settledPanel('Fact proposals');
    expect(within(panel).getByText(/this proposal carries no fact request/i)).toBeTruthy();
    expect(within(panel).queryByText('fp-orphan')).toBeNull();
  });
});

/**
 * A list that disagrees with the tally its own handler computed for it is not
 * the complete collection, and must not be drawn as one.
 */
describe('AutomationsPage list completeness', () => {
  it('says a job list is partial when it does not match the count beside it', async () => {
    stubAutomation({
      status: scheduler(measured(0), measured(0)),
      jobs: jobsBody([job('memory-curator', 'Memory curator')], 4),
    });
    renderAutomations();

    const panel = await settledPanel('Jobs');
    // The row is real and still shown; what it stops being is the whole list.
    expect(within(panel).getByText('Memory curator')).toBeTruthy();
    expect(within(panel).getByRole('status').textContent).toContain(
      'the daemon counted 4 jobs and sent 1',
    );
  });

  it('still reports an empty job list as empty when the count agrees', async () => {
    // Jobs are a configured list with no review queue behind them, so a
    // coherent empty body really is an empty list and says so.
    stubAutomation({
      status: scheduler(measured(0), measured(0)),
      jobs: jobsBody([]),
    });
    renderAutomations();

    const panel = await settledPanel('Jobs');
    expect(within(panel).getByText(/no automation jobs defined/i)).toBeTruthy();
    expect(within(panel).queryByRole('status')).toBeNull();
  });

  it('names the request cap when the proposal page is full', async () => {
    // `coerce_limit(params.limit, 50, 200)`: this page sends no limit, so a
    // response holding exactly 50 is a page and not a total.
    const page = Array.from({ length: 50 }, (_, i) => proposal(`fp-${i}`, `Fact ${i}.`));
    stubAutomation({
      status: scheduler(measured(50), measured(0)),
      'fact-proposals': proposalsBody(page),
    });
    renderAutomations();

    const panel = await settledPanel('Fact proposals');
    expect(within(panel).getByRole('status').textContent).toContain(
      'this is the first 50 proposals, the request cap, so there may be more',
    );
    expect(within(panel).getByText('Fact 0.')).toBeTruthy();
  });

  it('refuses a page holding more rows than the cap it ran under', async () => {
    // The query cannot outrun its own limit, so this is an incoherent body
    // rather than a full page, and saying "the first 2" would understate it.
    stubAutomation({
      status: scheduler(measured(3), measured(0)),
      'fact-proposals': proposalsBody(
        [proposal('fp-0', 'One.'), proposal('fp-1', 'Two.'), proposal('fp-2', 'Three.')],
        { limit: 2 },
      ),
    });
    renderAutomations();

    const panel = await settledPanel('Fact proposals');
    expect(within(panel).getByRole('status').textContent).toContain(
      'the daemon sent 3 proposals under a request cap of 2',
    );
  });

  it('does not call a full page partial when it is under the cap', async () => {
    stubAutomation({
      status: scheduler(measured(2), measured(0)),
      'fact-proposals': proposalsBody([proposal('fp-0', 'One.'), proposal('fp-1', 'Two.')]),
    });
    renderAutomations();

    const panel = await settledPanel('Fact proposals');
    expect(within(panel).queryByRole('status')).toBeNull();
    expect(within(panel).getByText('One.')).toBeTruthy();
  });
});

/**
 * The scheduler's `pending_review` is the authority on whether anything awaits
 * human approval, so a card may only call its queue empty when that authority
 * measured it.
 *
 * The two reads count different populations — the list routes return every
 * state under a cap, `pending_review` counts what awaits approval — so nothing
 * here compares their numbers. What it uses is the containment: a pending item
 * is one of the items the list enumerates.
 */
describe('AutomationsPage queue agreement', () => {
  it('will not call the proposal queue empty while the scheduler could not read it', async () => {
    stubAutomation({
      status: scheduler(unreadable('the project fact authority is mid-migration'), measured(0)),
      'fact-proposals': proposalsBody([]),
    });
    renderAutomations();

    const panel = await settledPanel('Fact proposals');
    expect(within(panel).queryByText(/no pending fact proposals/i)).toBeNull();
    expect(within(panel).getByRole('status').textContent).toContain(
      'whether the fact proposals queue is empty is unknown: the project fact authority is mid-migration',
    );
  });

  it('reports the disagreement when the queue is measured non-empty and the list is not', async () => {
    // Pending proposals are a subset of the proposals the list enumerates, so
    // these two readings cannot both be true and neither is chosen.
    stubAutomation({
      status: scheduler(measured(3), measured(0)),
      'fact-proposals': proposalsBody([]),
    });
    renderAutomations();

    const panel = await settledPanel('Fact proposals');
    expect(within(panel).queryByText(/no pending fact proposals/i)).toBeNull();
    expect(within(panel).getByRole('status').textContent).toContain(
      'the scheduler counted 3 awaiting review',
    );
  });

  it('says the queue is empty when both reads agree it is', async () => {
    stubAutomation({
      status: scheduler(measured(0), measured(0)),
      'fact-proposals': proposalsBody([]),
    });
    renderAutomations();

    const panel = await settledPanel('Fact proposals');
    expect(within(panel).getByText(/no pending fact proposals/i)).toBeTruthy();
    expect(within(panel).queryByRole('status')).toBeNull();
  });

  it('applies the same rule to managed skills', async () => {
    stubAutomation({
      status: scheduler(measured(0), unreadable('the managed skill store could not be opened')),
      skills: skillsBody([]),
    });
    renderAutomations();

    const panel = await settledPanel('Managed skills');
    expect(within(panel).queryByText(/no managed skills/i)).toBeNull();
    expect(within(panel).getByRole('status').textContent).toContain(
      'whether the skill drafts queue is empty is unknown',
    );
  });

  it('withholds the empty claim when the scheduler read itself never landed', async () => {
    // The cards render independently of the scheduler panel, so a failed
    // scheduler read has to reach them as an unreadable queue rather than as a
    // missing argument that defaults to "nothing is waiting".
    stubAutomation({
      status: () => new Response('nope', { status: 500 }),
      'fact-proposals': proposalsBody([]),
    });
    renderAutomations();

    const panel = await settledPanel('Fact proposals');
    expect(within(panel).queryByText(/no pending fact proposals/i)).toBeNull();
    expect(within(panel).getByRole('status').textContent).toContain(
      'the scheduler read failed (HTTP 500)',
    );
  });
});

/**
 * One panel, once its own read has returned.
 *
 * The region is in the tree from the first render, so `findByRole` matches it
 * while the read behind it is still in flight and every assertion would sample
 * the loading state. The four panels settle independently, so waiting on a
 * neighbour — the way the scheduler tests wait on a tile — proves nothing here.
 */
async function settledPanel(name: string): Promise<HTMLElement> {
  const panel = await screen.findByRole('region', { name });
  await waitFor(() => {
    expect(panel.querySelector('[data-state="loading"]')).toBeNull();
  });
  return panel;
}

/** The domain state a panel settled on, or null when it rendered content. */
function panelState(panel: HTMLElement): string | null {
  return panel.querySelector('[data-state]')?.getAttribute('data-state') ?? null;
}

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

/* --- Wire-true list bodies -------------------------------------------------
 *
 * Each builder emits exactly the `json!` literal its handler writes, including
 * the tally the handler derives from the very vector it serializes. Tests that
 * need a body to disagree with its own tally pass `count` explicitly, which is
 * the only way to express a truncated read.
 */

/** `automation_jobs_api::list` → `{jobs, count}`. */
function jobsBody(jobs: unknown[], count = jobs.length): Record<string, unknown> {
  return { jobs, count };
}

function job(id: string, name: string): Record<string, unknown> {
  return { id, name, schedule: '0 3 * * *', enabled: true, interval_secs: null };
}

/** `automation_skills_api::list` → `{profile_root, skills_root, count, skills, …}`. */
function skillsBody(skills: unknown[], count = skills.length): Record<string, unknown> {
  return {
    profile_root: '/home/x/.tracedecay',
    skills_root: '/home/x/.tracedecay/managed-skills',
    count,
    skills,
    skill_metadata: [],
    usage_summaries: [],
    stale_recommendations: [],
    improvement_recommendations: [],
  };
}

function skill(id: string, title: string, state = 'active'): Record<string, unknown> {
  return {
    metadata: { id, title, summary: `${title}.`, category: 'dev', state, pinned: false },
    body_markdown: `# ${title}`,
    support_files: [],
  };
}

/** `automation_fact_proposals_api::list` → `{proposals, count, limit, error}`.
 * The page sends no `limit`, so the handler's own default of 50 is the cap
 * every one of these bodies is read under. */
function proposalsBody(
  proposals: unknown[],
  options: { count?: number; limit?: number } = {},
): Record<string, unknown> {
  return {
    proposals,
    count: options.count ?? proposals.length,
    limit: options.limit ?? 50,
    error: '',
  };
}

function proposal(id: string, content: string | null): Record<string, unknown> {
  const row: Record<string, unknown> = {
    schema_version: 1,
    proposal_id: id,
    run_id: 'session-reflector-0',
    state: 'pending_approval',
  };
  // `add_fact_request` carries `skip_serializing_if = "Option::is_none"`, so a
  // record without one omits the key rather than sending null.
  if (content !== null) row['add_fact_request'] = { content, category: 'preference' };
  return row;
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
 * `skills`, `fact-proposals`. Anything the test does not name answers a
 * wire-true single-row body rather than failing, so a queue assertion never
 * trips over an unrelated panel. Returns the mock so a test can assert what was
 * actually requested.
 *
 * The list fallbacks carry a row on purpose. An empty list is no longer inert
 * scaffolding — the skills and proposal cards consult the scheduler's queue
 * before they will call themselves empty, so an empty fallback beside a
 * measured queue would render a contradiction notice in every unrelated test.
 * A row keeps those panels out of the way, which is what a fallback is for.
 */
function stubAutomation(replies: Record<string, Reply>) {
  const fallbacks: Record<string, unknown> = {
    jobs: jobsBody([job('nightly-sweep', 'Nightly sweep')]),
    skills: skillsBody([skill('code-slop', 'Code Slop Cleanup')]),
    'fact-proposals': proposalsBody([proposal('fp-1', 'A recorded project fact.')]),
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
