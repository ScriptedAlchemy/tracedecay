// LEGACY BOUNDARY — pending envelope migration.
// These schemas describe the pre-envelope plugin JSON endpoints
// (`/api/plugins/*`, `/api/projects`), NOT the DashboardEnvelopeV1 wire surface
// in `../../contracts/generated.ts`. They are hand-matched to their Rust
// producers and remain until these routes move to typed envelopes; new
// envelope-backed reads must use the single wire boundary in `contracts/`.
import { z } from 'zod';

/** Wire-true shapes for /api/plugins/savings/overview
 * (src/dashboard/savings_api.rs). */

const SavingsSumSchema = z
  .object({ saved_tokens: z.number(), calls: z.number() })
  .passthrough();

export const SavingsOverviewPayloadSchema = z
  .object({
    savings: z
      .object({
        available: z.boolean(),
        ledger: z
          .object({
            today: SavingsSumSchema,
            last_7d: SavingsSumSchema,
            last_30d: SavingsSumSchema,
            all_time: SavingsSumSchema,
          })
          .passthrough()
          .optional(),
        lifetime_counters: z
          .object({
            total_tokens_saved: z.number().optional(),
            projects: z
              .array(
                z
                  .object({
                    path: z.string().nullable().optional(),
                    tokens_saved: z.number().nullable().optional(),
                  })
                  .passthrough(),
              )
              .optional(),
          })
          .passthrough()
          .optional(),
      })
      .passthrough(),
    turns: z
      .object({
        available: z.boolean(),
        turn_count: z.number().optional(),
        total_cost_usd: z.number().optional(),
        total_tokens: z.number().optional(),
        cost_basis: z.string().optional(),
      })
      .passthrough(),
    sessions: z.object({ available: z.boolean() }).passthrough(),
    pricing: z
      .object({
        source: z.unknown().optional(),
        offline: z.unknown().optional(),
        model_count: z.unknown().optional(),
      })
      .passthrough(),
  })
  .passthrough();
export type SavingsOverviewPayload = z.infer<typeof SavingsOverviewPayloadSchema>;
