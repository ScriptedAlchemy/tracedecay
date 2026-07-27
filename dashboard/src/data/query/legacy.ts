import { z } from 'zod';
import type { WireSchema } from './wireSchema.ts';

/** Result for the legacy (pre-envelope) JSON endpoints. These are the
 * compatibility surfaces the old dashboard consumed; they return plain
 * payloads. Transport failures become truthful states, never exceptions.
 * As families migrate to DashboardEnvelopeV1, callers switch to
 * fetchEnvelope and this helper shrinks. */
export type LegacyResult<T> =
  | { outcome: 'ok'; data: T }
  | { outcome: 'offline' }
  | { outcome: 'unauthorized' }
  | { outcome: 'denied' }
  | { outcome: 'error'; detail: string }
  | { outcome: 'unsupported_schema' };

export async function fetchLegacy<T>(
  url: string,
  schema: WireSchema<T>,
  init?: RequestInit,
): Promise<LegacyResult<T>> {
  let response: Response;
  try {
    response = await fetch(url, { headers: { accept: 'application/json' }, ...init });
  } catch {
    return { outcome: 'offline' };
  }
  // An authorization refusal is its own reading, not an error carrying a
  // status code. 401 means the daemon accepted no identity for this read and
  // 403 means it knows the identity and will not serve this scope — two
  // different next actions for the reader, and neither one is "retry".
  if (response.status === 401) return { outcome: 'unauthorized' };
  if (response.status === 403) return { outcome: 'denied' };
  if (!response.ok) {
    return { outcome: 'error', detail: `HTTP ${response.status}` };
  }
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    return { outcome: 'unsupported_schema' };
  }
  const parsed = schema.safeParse(body);
  if (!parsed.success) return { outcome: 'unsupported_schema' };
  return { outcome: 'ok', data: parsed.data };
}

/** Loose object schema for legacy payloads we render generically. */
export const AnyObject = z.record(z.string(), z.unknown());
export type AnyObj = z.infer<typeof AnyObject>;

/* ---- typed slices of the legacy surfaces the workspaces consume ---- */

export const ProjectSchema = z
  .object({
    id: z.string().optional(),
    project_id: z.string().optional(),
    name: z.string().optional(),
    root: z.string().optional(),
    path: z.string().optional(),
  })
  .passthrough();
export const ProjectsSchema = z.preprocess(
  (value) => (Array.isArray(value) ? { projects: value } : value),
  z.object({ projects: z.array(ProjectSchema).optional() }).passthrough(),
);

export const LcmOverviewSchema = AnyObject;
export const LcmSessionsSchema = AnyObject;
export const MemoryOverviewSchema = AnyObject;
export const GraphOverviewSchema = AnyObject;
export const SavingsOverviewSchema = AnyObject;
export const AutomationStatusSchema = AnyObject;
export const SettingsSchema = AnyObject;
