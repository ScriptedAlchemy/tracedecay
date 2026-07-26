/**
 * Explorer's wire names.
 *
 * The query-run shapes below are re-exported from the generated barrel, not
 * declared. They used to be hand-written here under *exactly* the names the
 * generator already emits — `ExplorerQueryRunSchema`,
 * `ExplorerSourceProgressSchema`, `ExplorerSourceIdSchema` — so which contract
 * a caller validated against depended on which module it happened to import.
 * `dashboard/e2e/axe-explorer.ts` imported this one, and had therefore been
 * checking the accessibility harness against a shape the server never promised.
 *
 * The copy had already drifted, in the direction that throws in front of a
 * user rather than the direction that merely under-reads:
 *
 *  - `freshness` was a five-member `z.enum`. Rust types it as a plain string,
 *    so the first freshness value added on the server would have failed the
 *    parse and taken the whole Explorer result down, rather than arriving as an
 *    unfamiliar string this dashboard could report verbatim.
 *  - `ExplorerSourceProgress` was a discriminated union pinning `page: null` on
 *    four of the five outcomes. Rust models `page` as one nullable field on a
 *    flat struct and encodes no such per-outcome invariant, so a server that
 *    ever attached a partial page to an `error` source would have had a real,
 *    populated reading rejected as malformed.
 *
 * Both are the `authority_scope` failure again: a frontend asserting a shape
 * the backend never agreed to. The generated contract is the agreement, so the
 * aliases below simply point at it.
 *
 * `ExplorerSessionSize` and `ExplorerReadContext` are still declared by hand
 * because their routes have no Rust wire type to generate from — the same
 * uncontracted-route gap the automation surface had. They are the honest
 * remainder here, not a preference, and they should be deleted the moment those
 * two routes enter the contract catalog.
 */
import { z } from 'zod';
import { AnyObject } from '../../data/query/legacy.ts';

export {
  ExplorerQueryRunSchema,
  ExplorerSourceProgressSchema,
  ExplorerSourceIdSchema,
  ExplorerResultPageSchema,
  ExplorerSourceOutcomeSchema,
} from '../../contracts/generated.ts';

export type {
  ExplorerQueryRunV1 as ExplorerQueryRun,
  ExplorerSourceProgressV1 as ExplorerSourceProgress,
  ExplorerSourceIdV1 as ExplorerSourceId,
  ExplorerResultPageV1 as ExplorerResultPage,
} from '../../contracts/generated.ts';

const SessionCountsSchema = z
  .object({
    message_count: z.number().int().nonnegative(),
    summary_node_count: z.number().int().nonnegative(),
    token_estimate_total: z.number().int().nonnegative(),
    summary_token_count: z.number().int().nonnegative(),
    source_token_count: z.number().int().nonnegative(),
  })
  .passthrough();

export const ExplorerSessionSizeSchema = z
  .object({
    session_id: z.string(),
    storage_scope: z.string(),
    counts: SessionCountsSchema,
  })
  .passthrough();

export const ExplorerReadContextSchema = z
  .object({
    session_id: z.string(),
    storage_scope: z.string(),
    limit: z.number().int().positive(),
    offset: z.number().int().nonnegative(),
    order: z.enum(['asc', 'desc']),
    counts: SessionCountsSchema,
    messages: z.array(AnyObject),
    summary_nodes: z.array(AnyObject),
    has_more: z.boolean(),
    has_more_messages: z.boolean(),
    has_more_summary_nodes: z.boolean(),
  })
  .passthrough();

export type ExplorerSessionSize = z.infer<typeof ExplorerSessionSizeSchema>;
export type ExplorerReadContext = z.infer<typeof ExplorerReadContextSchema>;
