/**
 * UNCONTRACTED — `GET /api/plugins/analytics/overview`.
 *
 * Producer: `src/dashboard/analytics_api.rs`, returning `serde_json::Value`.
 * Nothing here is generated; see `./README.md`.
 *
 * Read by the Brain workspace through the project-scoped gateway. The Agents
 * workspace reads the sibling `usage`/`hints`/`diagnostics` routes with
 * page-local schemas, which are mirrored in
 * `workspaces/endpoint-fixtures.test.ts` rather than declared here — they are
 * the same gap, one layer further from being closed.
 */
import { z } from 'zod';

export const AnalyticsOverviewPayloadSchema = z
  .object({
    available: z.boolean().optional(),
    usage: z
      .object({
        available: z.boolean().optional(),
        event_count: z.number().optional(),
        message_count: z.number().optional(),
        by_category: z
          .array(
            z
              .object({ category: z.string(), events: z.number(), kind: z.string().optional() })
              .passthrough(),
          )
          .optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();
export type AnalyticsOverviewPayload = z.infer<typeof AnalyticsOverviewPayloadSchema>;
