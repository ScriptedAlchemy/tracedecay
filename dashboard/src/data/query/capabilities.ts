/**
 * `/api/capabilities`, and the multi-root capability inside it.
 *
 * The daemon reports a typed multi-root capability on this route:
 * `mod.rs::capabilities` builds it from `state.authorized_scope_set` as the
 * generated `MultiRootCapabilityV1`, which is a two-state union, `mounted`,
 * carrying the scope-set id, revision, digest and root count, or
 * `unavailable`, carrying the daemon's own sentence about why.
 *
 * What is deliberately NOT here is a federated query. `MultiRootQueryReadModelV1`
 * is generated, but no route serves it: there is no endpoint that runs a query
 * across a scope set, so a root pivot would have nothing behind it. The
 * capability says a scope set is mounted; it does not say this dashboard can
 * query across it, and {@link multiRootReading} keeps those two claims apart
 * rather than letting the first imply the second.
 */
import { useQuery } from '@tanstack/react-query';
import { z } from 'zod';
import { MultiRootCapabilityV1Schema } from '../../contracts/generated.ts';
import type { MultiRootCapabilityV1 } from '../../contracts/generated.ts';
import { fetchPayload, type PayloadResult } from './payload.ts';

/** The route, named once. */
const CAPABILITIES_URL = '/api/capabilities';

/**
 * Only the members this dashboard reads. `passthrough` keeps the rest of the
 * bundle intact for the fixture gate, which parses the same body against
 * `CapabilitiesSchema`.
 */
const CapabilitiesReadSchema = z
  .object({ multi_root: MultiRootCapabilityV1Schema })
  .passthrough();

export type CapabilitiesRead = z.infer<typeof CapabilitiesReadSchema>;

/** `GET /api/capabilities`. Unscoped: the bundle describes the daemon and the
 * project it was launched for, not the selected scope, so it is one entry. */
export function useCapabilities() {
  return useQuery<PayloadResult<CapabilitiesRead>>({
    queryKey: ['capabilities'],
    queryFn: ({ signal }) => fetchPayload(CAPABILITIES_URL, CapabilitiesReadSchema, { signal }),
    // Capability discovery is daemon-wide and shared by every fleet surface.
    // Keep the one answer warm while those surfaces mount so separate
    // observers do not turn a single page read into duplicate requests.
    refetchInterval: false,
    staleTime: 60_000,
  });
}

/** What may be said about multi-root, given a capability bundle. */
export type MultiRootReading =
  | { state: 'unavailable'; reason: string }
  | {
      state: 'mounted';
      scopeSetId: string;
      revision: number;
      digest: string;
      rootCount: number;
      /** Whether a query can be run across those roots. Always false today: no
       * route serves `MultiRootQueryReadModelV1`. Named rather than assumed so
       * the surface states the boundary instead of implying a pivot exists. */
      federatedQueryMounted: false;
    };

export function multiRootReading(capability: MultiRootCapabilityV1): MultiRootReading {
  switch (capability.status) {
    case 'unavailable':
      return { state: 'unavailable', reason: capability.reason };
    case 'mounted':
      return {
        state: 'mounted',
        scopeSetId: capability.scope_set_id,
        revision: capability.revision,
        digest: capability.scope_set_digest,
        rootCount: capability.root_count,
        federatedQueryMounted: false,
      };
    default: {
      const exhaustive: never = capability;
      return exhaustive;
    }
  }
}
