import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { RunHistory } from './RunHistory.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

/**
 * The run history is the automation runtime's own ledger read back:
 * `/api/automation/runs` serves the tail of the JSONL ledger and each row's
 * artifacts come from `/api/automation/runs/{id}/artifacts`, which also carries
 * the daemon's chain-integrity verdict. These tests hold the surface to the
 * same truthfulness rules as the rest of the page: an empty ledger is an empty
 * ledger, an unread one is a blocked state, artifacts are fetched only when a
 * run is opened, and the integrity verdict is the server's word verbatim.
 */
describe('RunHistory', () => {
  it('reports an empty ledger as a ledger with no runs, not a blocked read', async () => {
    stubRuns({ runs: runsBody([]) });
    renderRunHistory();
    expect(
      await screen.findByText(/no automation runs are recorded/i),
    ).toBeTruthy();
  });

  it('renders each run with its own status word and review tally', async () => {
    stubRuns({
      runs: runsBody([
        run('run-1', { task: 'memory_curator', status: 'applied', accepted: 3, reviewed: 4 }),
        run('run-2', { task: 'skill_writing', status: 'failed', error: 'backend refused' }),
      ]),
    });
    renderRunHistory();
    await screen.findByText('memory_curator');

    expect(screen.getByText('applied')).toBeTruthy();
    expect(screen.getByText('3/4 accepted')).toBeTruthy();
    expect(screen.getByText('failed')).toBeTruthy();
    expect(screen.getByText('backend refused')).toBeTruthy();
  });

  it('fetches artifacts only when a run is opened, and prints the daemon integrity verdict', async () => {
    const fetchMock = stubRuns({
      runs: runsBody([
        run('run-1', {
          task: 'memory_curator',
          status: 'applied',
          artifactKinds: ['traces'],
        }),
      ]),
      artifacts: artifactsBody('run-1', 'ledger_publication_mismatch'),
    });
    renderRunHistory();
    const row = await screen.findByRole('button', { name: /memory_curator/ });

    // No artifact request before the disclosure opens.
    expect(
      fetchMock.mock.calls.some(([url]) => String(url).includes('/artifacts')),
    ).toBe(false);

    await userEvent.click(row);
    await screen.findByText(/chain integrity: ledger_publication_mismatch/i);
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([url]) =>
          String(url).endsWith('/api/automation/runs/run-1/artifacts'),
        ),
      ).toBe(true),
    );
  });

  it('says when an opened run recorded no artifacts instead of issuing a read', async () => {
    const fetchMock = stubRuns({
      runs: runsBody([run('run-1', { task: 'session_reflection', status: 'completed' })]),
    });
    renderRunHistory();
    await userEvent.click(await screen.findByRole('button', { name: /session_reflection/ }));

    expect(await screen.findByText(/recorded no artifacts/i)).toBeTruthy();
    expect(
      fetchMock.mock.calls.some(([url]) => String(url).includes('/artifacts')),
    ).toBe(false);
  });

  it('marks a capped page as the newest slice rather than the whole ledger', async () => {
    const rows = Array.from({ length: 50 }, (_, index) =>
      run(`run-${index}`, { task: 'memory_curator', status: 'applied' }),
    );
    stubRuns({ runs: { runs: rows, count: 50, limit: 50, error: '' } });
    renderRunHistory();
    expect(await screen.findByText(/the newest 50 runs, the request cap/i)).toBeTruthy();
  });
});

type Reply = unknown;

function runsBody(rows: unknown[]) {
  return { runs: rows, count: rows.length, limit: 50, error: '' };
}

function run(
  id: string,
  options: {
    task: string;
    status: string;
    accepted?: number;
    reviewed?: number;
    error?: string;
    artifactKinds?: string[];
  },
) {
  return {
    run_id: id,
    task: options.task,
    trigger: 'manual',
    backend: 'claude',
    model: null,
    status: options.status,
    reviewed_count: options.reviewed ?? 0,
    accepted_count: options.accepted ?? 0,
    rejected_count: 0,
    skipped_count: 0,
    error: options.error ?? null,
    started_at: '1754000000',
    completed_at: '1754000060',
    artifact_kinds: options.artifactKinds ?? [],
  };
}

function artifactsBody(runId: string, integrity: string) {
  return {
    run_id: runId,
    artifacts: [
      {
        kind: 'traces',
        path: `runs/${runId}/traces.json`,
        sha256: 'a'.repeat(64),
        created_at: '1754000060',
      },
    ],
    artifact_chain: {
      expected_kinds: ['traces', 'feedback'],
      present_kinds: ['traces'],
      metadata_complete: false,
      complete: false,
      integrity_status: integrity,
    },
    count: 1,
    error: '',
  };
}

function stubRuns(replies: { runs: Reply; artifacts?: Reply }) {
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes('/artifacts')) {
      return jsonResponse(replies.artifacts ?? {});
    }
    return jsonResponse(replies.runs);
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function renderRunHistory() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <RunHistory />
    </QueryClientProvider>,
  );
}
