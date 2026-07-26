/**
 * UNCONTRACTED — `GET /api/plugins/savings/overview`.
 *
 * Producer: `src/dashboard/savings_api.rs`, returning `serde_json::Value`.
 * Nothing here is generated; see `./README.md`.
 *
 * Note that the Rust contract catalog does carry a `CostsReadModelV1`
 * (`dashboard/contract_schema.rs`), but it belongs to the envelope-backed
 * costs read model, not to this pre-envelope plugin route. The Costs workspace
 * still reads this one, so the gap is real.
 */
import { z } from 'zod';

const SavingsSumSchema = z
  .object({ saved_tokens: z.number(), calls: z.number() })
  .passthrough();

export const SavingsOverviewPayloadSchema = z
  .object({
    savings: z
      .object({
        available: z.boolean(),
        error: z.string().optional(),
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
            project_total: z.number().optional(),
            projects_limit: z.number().optional(),
            projects_truncated: z.boolean().optional(),
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
        error: z.string().optional(),
        turn_count: z.number().optional(),
        total_cost_usd: z.number().optional(),
        total_tokens: z.number().optional(),
        cost_basis: z.string().optional(),
      })
      .passthrough(),
    /**
     * The session ledger's own token accounting — a DIFFERENT denominator from
     * `turns`. `turns` is the priced turn ledger (57,704 turns on the owner's
     * profile); this counts every message the session store holds (1.75M), and
     * splits them by whether a provider reported usage, the figure was
     * estimated, or the model could not be identified at all. Both are true;
     * they answer different questions, and the page has to say which is which.
     */
    sessions: z
      .object({
        available: z.boolean(),
        error: z.string().optional(),
        cost_basis: z.string().optional(),
        scope: z.string().optional(),
        session_count: z.number().optional(),
        model_count: z.number().optional(),
        messages: z.number().optional(),
        usage_messages: z.number().optional(),
        estimated_messages: z.number().optional(),
        tokenized_messages: z.number().optional(),
        unknown_model_messages: z.number().optional(),
        actual: z
          .object({
            input_tokens: z.number().optional(),
            output_tokens: z.number().optional(),
            cache_read_tokens: z.number().optional(),
            cache_write_tokens: z.number().optional(),
          })
          .passthrough()
          .optional(),
        estimated: z
          .object({
            input_tokens: z.number().optional(),
            output_tokens: z.number().optional(),
          })
          .passthrough()
          .optional(),
      })
      .passthrough(),
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
