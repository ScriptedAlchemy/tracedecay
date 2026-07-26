/**
 * UNCONTRACTED — the two Explorer session routes.
 *
 *   GET /api/explorer/session/{id}/size
 *   GET /api/explorer/session/{id}/read
 *
 * Producer: `src/dashboard/explorer_api.rs`. Nothing here is generated; see
 * `./README.md`.
 *
 * This is the narrowest gap in the directory and the best one to close first:
 * `/api/explorer/query` on the same Rust module IS contracted
 * (`ExplorerQueryRunV1` is in `DashboardContractCatalogV1`), so these two
 * siblings are the only reason the Explorer workspace reads anything
 * hand-written at all. Give them a `JsonSchema` DTO and this file goes away.
 */
import { z } from 'zod';

import { AnyObject } from '../../data/query/legacy.ts';

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
export type ExplorerSessionSize = z.infer<typeof ExplorerSessionSizeSchema>;

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
export type ExplorerReadContext = z.infer<typeof ExplorerReadContextSchema>;
