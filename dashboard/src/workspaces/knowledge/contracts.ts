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

export const MemoryOverviewPayloadSchema = z
  .object({
    query: z.string().optional(),
    holographic: z
      .object({
        error: z.string().optional(),
        facts: z.array(FactRowSchema).optional(),
        entities: z.array(EntityRowSchema).optional(),
        overview: z
          .object({
            facts: z.number().optional(),
            entities: z.number().optional(),
            banks: z.number().optional(),
            hrr_coverage: z.number().optional(),
            trust_histogram: z.array(TrustBucketSchema).optional(),
          })
          .passthrough()
          .nullable()
          .optional(),
      })
      .passthrough(),
  })
  .passthrough();
export type MemoryOverviewPayload = z.infer<typeof MemoryOverviewPayloadSchema>;
