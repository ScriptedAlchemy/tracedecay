import { z } from 'zod';
import { AnyObject } from '../../data/query/legacy.ts';

export const ExplorerSourceIdSchema = z.enum(['code_graph', 'sessions', 'knowledge']);

const CoverageSchema = z
  .object({
    completeness: z.enum(['complete', 'partial', 'unknown', 'unsupported']),
    eligible: z.number().int().nonnegative().nullable(),
    examined: z.number().int().nonnegative().nullable(),
    matched: z.number().int().nonnegative().nullable(),
    excluded: z.number().int().nonnegative().nullable(),
    omitted: z.number().int().nonnegative().nullable(),
    unknown: z.number().int().nonnegative().nullable(),
    denominator: z.number().int().nonnegative().nullable(),
    unit: z.string().nullable(),
    omission_reasons: z.array(z.string()),
  })
  .passthrough();

const ResultPageSchema = z
  .object({
    offset: z.number().int().nonnegative(),
    limit: z.number().int().positive(),
    total: z.number().int().nonnegative().nullable(),
    next_offset: z.number().int().nonnegative().nullable(),
    rows: z.array(AnyObject),
    metadata: AnyObject,
  })
  .passthrough();

const SourceBaseSchema = z.object({
  source_id: ExplorerSourceIdSchema,
  source_label: z.string(),
  phase: z.enum(['queued', 'reading', 'completed', 'cancelled']),
  completed_units: z.number().int().nonnegative().nullable(),
  total_units: z.number().int().nonnegative().nullable(),
  coverage: CoverageSchema,
  freshness: z.enum(['fresh', 'stale', 'unknown', 'absent', 'unsupported']),
  watermark: z.string().nullable(),
  error_code: z.string().nullable(),
  message: z.string().nullable(),
});

export const ExplorerSourceProgressSchema = z.discriminatedUnion('outcome', [
  SourceBaseSchema.extend({
    outcome: z.literal('pending'),
    page: z.null(),
  }),
  SourceBaseSchema.extend({
    outcome: z.literal('ready'),
    page: ResultPageSchema,
  }),
  SourceBaseSchema.extend({
    outcome: z.literal('unavailable'),
    page: z.null(),
  }),
  SourceBaseSchema.extend({
    outcome: z.literal('error'),
    page: z.null(),
  }),
  SourceBaseSchema.extend({
    outcome: z.literal('cancelled'),
    page: z.null(),
  }),
]);

export const ExplorerQueryRunSchema = z
  .object({
    run_id: z.string().min(1),
    request: z
      .object({
        query: z.string().min(1),
        limit: z.number().int().positive(),
        offset: z.number().int().nonnegative(),
      })
      .passthrough(),
    request_revision: z.string(),
    plan_revision: z.string(),
    merge_revision: z.string(),
    required_source_ids: z.array(ExplorerSourceIdSchema),
    ordering_policy: z.string(),
    explanation: z.string(),
    submitted_at_micros: z.number().int(),
    completed_at_micros: z.number().int().nullable(),
    elapsed_micros: z.number().int().nonnegative(),
    state: z.enum(['pending', 'completed', 'partial', 'cancelled', 'error']),
    finality: z.enum(['pending', 'complete', 'partial', 'cancelled', 'error']),
    sources: z.array(ExplorerSourceProgressSchema),
  })
  .passthrough();

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

export type ExplorerSourceId = z.infer<typeof ExplorerSourceIdSchema>;
export type ExplorerSourceProgress = z.infer<typeof ExplorerSourceProgressSchema>;
export type ExplorerQueryRun = z.infer<typeof ExplorerQueryRunSchema>;
export type ExplorerSessionSize = z.infer<typeof ExplorerSessionSizeSchema>;
export type ExplorerReadContext = z.infer<typeof ExplorerReadContextSchema>;
