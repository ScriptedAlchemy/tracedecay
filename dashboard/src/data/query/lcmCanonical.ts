import { z } from 'zod';
import {
  ExplorerReadContextV1Schema,
  ExplorerSessionCountsV1Schema,
  ExplorerSessionSizeV1Schema,
  LcmSessionCountsV1Schema,
  LcmSessionPayloadV1Schema,
  LcmSummaryNodeV1Schema,
} from '../../contracts/generated.ts';

/**
 * Cursor-backed LCM reads landed after the last checked-in contract generation.
 * These overlays narrow the temporary wire delta at the consumers: token totals
 * are unknown when the canonical describe authority does not expose them, and
 * session pages carry an opaque next cursor instead of offset/order fields.
 *
 * Delete these overlays when the generated contracts are refreshed.
 */
const CanonicalLcmSessionCountsV1Schema = LcmSessionCountsV1Schema.omit({
  source_token_count: true,
  summary_token_count: true,
  token_estimate_total: true,
}).extend({
  source_token_count: z.number().int().nullable(),
  summary_token_count: z.number().int().nullable(),
  token_estimate_total: z.number().int().nullable(),
});

const CanonicalExplorerSessionCountsV1Schema = ExplorerSessionCountsV1Schema.omit({
  source_token_count: true,
  summary_token_count: true,
  token_estimate_total: true,
}).extend({
  source_token_count: z.number().int().nullable(),
  summary_token_count: z.number().int().nullable(),
  token_estimate_total: z.number().int().nullable(),
});

export const CanonicalLcmSummaryNodeV1Schema = LcmSummaryNodeV1Schema.omit({
  source_token_count: true,
  token_count: true,
}).extend({
  source_token_count: z.number().int().nullable(),
  token_count: z.number().int().nullable(),
});

export const CanonicalLcmSessionPayloadV1Schema = LcmSessionPayloadV1Schema.omit({
  counts: true,
  offset: true,
  order: true,
  summary_nodes: true,
}).extend({
  counts: CanonicalLcmSessionCountsV1Schema,
  next_cursor: z.string().nullable(),
  summary_nodes: z.array(CanonicalLcmSummaryNodeV1Schema),
});
export type CanonicalLcmSessionPayloadV1 = z.infer<
  typeof CanonicalLcmSessionPayloadV1Schema
>;
export type CanonicalLcmSummaryNodeV1 = z.infer<
  typeof CanonicalLcmSummaryNodeV1Schema
>;

export const CanonicalExplorerSessionSizeV1Schema = ExplorerSessionSizeV1Schema.omit({
  counts: true,
}).extend({
  counts: CanonicalExplorerSessionCountsV1Schema,
});
export type CanonicalExplorerSessionSizeV1 = z.infer<
  typeof CanonicalExplorerSessionSizeV1Schema
>;

export const CanonicalExplorerReadContextV1Schema = ExplorerReadContextV1Schema.omit({
  counts: true,
  summary_nodes: true,
}).extend({
  counts: CanonicalExplorerSessionCountsV1Schema,
  summary_nodes: z.array(CanonicalLcmSummaryNodeV1Schema),
});
export type CanonicalExplorerReadContextV1 = z.infer<
  typeof CanonicalExplorerReadContextV1Schema
>;
