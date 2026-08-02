/**
 * One entry, one period, for the two surfaces that read Doctor findings.
 *
 * Observatory's findings section and the nav rail's health dot both call
 * `/api/storage/findings`. They used to spell the key and the URL separately
 * and name different `refetchInterval`s — 30 seconds and 60. Because the key
 * was the same, React Query never honoured both: the shared entry polled on the
 * shorter of the two whenever both were mounted, so the rail's stated minute
 * was not a period that existed. These tests hold the entry to a single
 * definition and the poll to a single number.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, waitFor, cleanup } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DashboardEnvelopeV1Schema, StorageFindingsPayloadV1Schema } from '../../contracts/generated.ts';
import {
  STORAGE_FINDINGS_REFETCH_MS,
  STORAGE_FINDINGS_URL,
  storageFindingsKey,
  useStorageFindings,
} from './storageFindings.ts';
import { useScope } from '../scope/store.ts';

/** A wire-true findings envelope, validated by the generated contract below so
 * this fixture cannot drift into a shape the daemon never sends. */
function findingsBody() {
  return {
    schema_revision: 1,
    scope: { project_id: 'project.findings', storage_mode: 'project_local', store_root: '/p' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 100 },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
      eligible: 0,
      examined: 0,
      matched: null,
      excluded: null,
      omitted: 0,
      unknown: null,
      denominator: 0,
      unit: 'stores',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: 100, watermark: null },
    domain_state: 'ready',
    legal_actions: [],
    payload: {
      family_filter: 'storage',
      entries: [],
      report_coverage: null,
      remediations: [],
      known_families: ['storage'],
      note: 'canonical Doctor storage family contained no entries',
      kind_statuses: [],
    },
  };
}

let requested: string[] = [];

beforeEach(() => {
  requested = [];
  useScope.getState().selectAllProjects();
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      requested.push(String(input));
      return new Response(JSON.stringify(findingsBody()), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

/** Two independent readers, exactly as the rail and Observatory mount them. */
function Reader({ tag }: { tag: string }) {
  const query = useStorageFindings();
  return <p data-reader={tag} data-outcome={query.data?.outcome ?? 'pending'} />;
}

function renderBoth() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return {
    client,
    ...render(
      <QueryClientProvider client={client}>
        <Reader tag="rail" />
        <Reader tag="observatory" />
      </QueryClientProvider>,
    ),
  };
}

describe('the Doctor findings read, shared by two surfaces', () => {
  it('reads a body the generated contract accepts', () => {
    // Guards the two tests below: they distinguish decoded answers from
    // transport outcomes, which only means something while this fixture is one
    // the contract admits.
    expect(DashboardEnvelopeV1Schema(StorageFindingsPayloadV1Schema).safeParse(findingsBody()).success).toBe(
      true,
    );
  });

  it('is one cache entry and one request, not one per caller', async () => {
    const { client } = renderBoth();

    await waitFor(() =>
      expect(document.querySelectorAll('[data-outcome="envelope"]')).toHaveLength(2),
    );

    // Both readers answered, and the network was asked once. Two hand-written
    // call sites that ever disagreed about the key would show up here as two.
    expect(requested).toEqual([STORAGE_FINDINGS_URL]);
    expect(client.getQueryCache().findAll({ queryKey: ['storage', 'findings'] })).toHaveLength(1);
  });

  it('gives every observer of that entry the same period', async () => {
    const { client } = renderBoth();
    await waitFor(() =>
      expect(document.querySelectorAll('[data-outcome="envelope"]')).toHaveLength(2),
    );

    const entry = client.getQueryCache().find({ queryKey: storageFindingsKey({ kind: 'all' }) });
    const periods = entry?.observers.map((observer) => observer.options.refetchInterval) ?? [];

    // The assertion that fails on the old pair: two observers, two different
    // numbers, and the entry silently taking the smaller one.
    expect(periods).toHaveLength(2);
    expect(new Set(periods)).toEqual(new Set([STORAGE_FINDINGS_REFETCH_MS]));
  });

  it('keys by scope, so switching project does not answer with the last one', () => {
    expect(storageFindingsKey({ kind: 'all' })).toEqual(['storage', 'findings', 'all']);
    expect(
      storageFindingsKey({
        kind: 'project',
        projectId: 'proj_a',
        label: 'Project A',
        activation: 'active',
      }),
    ).toEqual(['storage', 'findings', 'project:proj_a']);
  });
});
