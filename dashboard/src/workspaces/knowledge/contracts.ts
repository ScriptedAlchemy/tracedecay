// LEGACY BOUNDARY — pending envelope migration.
// These schemas describe the pre-envelope plugin JSON endpoints
// (`/api/plugins/*`, `/api/projects`), NOT the DashboardEnvelopeV1 wire surface
// in `../../contracts/generated.ts`. They are hand-matched to their Rust
// producers and remain until these routes move to typed envelopes; new
// envelope-backed reads must use the single wire boundary in `contracts/`.
import { z } from 'zod';

/** Wire-true shapes for GET /api/plugins/holographic (memory overview).
 * Fact rows come from fact_summary_json in
 * src/dashboard/memory_service/facts.rs. */

export const FactRowSchema = z
  .object({
    fact_id: z.union([z.string(), z.number()]),
    trust_score: z.number(),
    retrieval_count: z.number().optional(),
    access_count: z.number().optional(),
    helpful_count: z.number().optional(),
    unhelpful_count: z.number().optional(),
    created_at: z.number().optional(),
    updated_at: z.number().optional(),
    last_recalled_at: z.number().nullable().optional(),
    has_hrr: z.number().optional(),
    content: z.string().optional(),
    category: z.string().optional(),
    tags: z.array(z.string()).optional(),
  })
  .passthrough();
export type FactRow = z.infer<typeof FactRowSchema>;

export const EntityRowSchema = z
  .object({
    entity_id: z.union([z.string(), z.number()]).nullable().optional(),
    name: z.string(),
    entity_type: z.string().nullable().optional(),
    fact_count: z.number().optional(),
  })
  .passthrough();
export type EntityRow = z.infer<typeof EntityRowSchema>;

export const TrustBucketSchema = z
  .object({ bucket: z.number(), label: z.string(), count: z.number() })
  .passthrough();

/** One per-category HRR coverage row from overview_payload in facts.rs. */
export const HrrCoverageRowSchema = z
  .object({
    category: z.string(),
    facts: z.number(),
    hrr_vectors: z.number(),
    /** Fraction 0..1 (basis points / 10_000 on the wire producer). */
    coverage: z.number(),
    bank_name: z.string().nullable().optional(),
    bank_fact_count: z.number().nullable().optional(),
    dim: z.number().nullable().optional(),
    updated_at: z.number().nullable().optional(),
    status: z.enum(['ready', 'missing_vectors', 'missing_bank', 'stale_bank']),
  })
  .passthrough();
export type HrrCoverageRow = z.infer<typeof HrrCoverageRowSchema>;

/** One `categories` row from overview_payload in facts.rs. */
export const CategoryCountSchema = z
  .object({ category: z.string(), count: z.number() })
  .passthrough();
export type CategoryCount = z.infer<typeof CategoryCountSchema>;

/** One `growth` point from overview_payload in facts.rs (period bucket, facts
 * added in that bucket, and the running total at that point). */
export const GrowthPointSchema = z
  .object({
    date: z.string(),
    facts: z.number(),
    cumulative_facts: z.number(),
  })
  .passthrough();
export type GrowthPoint = z.infer<typeof GrowthPointSchema>;

export const MemoryOverviewPayloadSchema = z
  .object({
    query: z.string().optional(),
    holographic: z
      .object({
        error: z.string().optional(),
        facts: z.array(FactRowSchema).optional(),
        entities: z.array(EntityRowSchema).optional(),
        reads: z
          .object({
            facts: z
              .object({ state: z.enum(['pending', 'ready', 'error']), error: z.string().optional() })
              .passthrough(),
            entities: z
              .object({ state: z.enum(['pending', 'ready', 'error']), error: z.string().optional() })
              .passthrough(),
            graph: z
              .object({ state: z.enum(['pending', 'ready', 'error']), error: z.string().optional() })
              .passthrough(),
          })
          .passthrough()
          .optional(),
        facts_coverage: z
          .object({
            completeness: z.literal('bounded'),
            limit: z.number(),
            query_applied_after_limit: z.boolean(),
          })
          .passthrough()
          .optional(),
        overview: z
          .object({
            facts: z.number().optional(),
            entities: z.number().optional(),
            banks: z.number().optional(),
            categories: z.array(CategoryCountSchema).optional(),
            hrr_coverage: z.array(HrrCoverageRowSchema).optional(),
            trust_histogram: z.array(TrustBucketSchema).optional(),
            growth: z.array(GrowthPointSchema).optional(),
          })
          .passthrough()
          .nullable()
          .optional(),
      })
      .passthrough(),
  })
  .passthrough();
export type MemoryOverviewPayload = z.infer<typeof MemoryOverviewPayloadSchema>;

export const FactDetailPayloadSchema = z
  .object({
    fact: FactRowSchema.nullable().optional(),
    error: z.string().optional(),
  })
  .passthrough();

/**
 * GET /api/plugins/holographic/status (src/dashboard/memory_api.rs `status`).
 *
 * Read here for one reason: `memory.trust_*_count` is the only trust
 * distribution a real store currently reports correctly. The overview's
 * `trust_histogram` is produced with row names of the form `trust-<n>` and
 * consumed with `parse::<usize>()`, so every bucket comes back zero.
 */
export const MemoryStatusSchema = z
  .object({
    exists: z.boolean().optional(),
    error: z.string().optional(),
    memory: z
      .object({
        fact_count: z.number().optional(),
        entity_count: z.number().optional(),
        bank_count: z.number().optional(),
        helpful_count: z.number().optional(),
        unhelpful_count: z.number().optional(),
        trust_0_025_count: z.number().optional(),
        trust_025_050_count: z.number().optional(),
        trust_050_075_count: z.number().optional(),
        trust_075_100_count: z.number().optional(),
        feedback_funnel: z
          .object({
            rated_fact_count: z.number().optional(),
            feedback_total: z.number().optional(),
            retrieved_fact_count: z.number().optional(),
            retrieval_count_total: z.number().optional(),
          })
          .passthrough()
          .optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();
export type MemoryStatusPayload = z.infer<typeof MemoryStatusSchema>;
