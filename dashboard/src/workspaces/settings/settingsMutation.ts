import { z } from 'zod';
import type { SettingsPayloadV1 } from '../../contracts/generated.ts';
import { readOnlyScopeRefusal, type ScopeWritability } from '../../data/scope/store.ts';
import {
  buildSettingsEditor,
  readSettingsEnvelope,
  settingsRevisionConflict,
  settingsRevisionId,
  type ProjectSettingsChangeSet,
  type SettingsScope,
  type SettingsValidationError,
  type UserSettingsChangeSet,
} from './settingsModel.ts';

export type SettingsMutationScope = SettingsScope;

export type SettingsMutationResult =
  | {
      readonly outcome: 'success';
      readonly scope: SettingsMutationScope;
      readonly payload: SettingsPayloadV1;
      readonly revisionId: string;
      readonly resyncRecommended: boolean;
      readonly restartRecommended: boolean;
      /** Canonical durable application receipt for project writes. */
      readonly effectReceipt: Readonly<Record<string, unknown>> | null;
    }
  | {
      readonly outcome: 'conflict';
      readonly expectedRevisionId: string;
      readonly actualRevisionId: string | null;
    }
  | {
      readonly outcome: 'validation';
      readonly detail: string;
      readonly errors: readonly SettingsValidationError[];
    }
  | {
      readonly outcome: 'offline' | 'error';
      readonly detail: string;
    }
  /** The authority that performs this write is not currently mounted. Kept
   * apart from `error`: nothing was attempted, and nothing changed. */
  | {
      readonly outcome: 'unavailable';
      readonly detail: string;
    }
  /** The scope declined the write before anything was sent. Not a failure of
   * the write — the absence of one, with the scope authority's own reason. */
  | {
      readonly outcome: 'not_dispatched';
      readonly detail: string;
    }
  /** The project gateway refused the PATCH because this project is not the
   * active one. Distinct from `error`: the request was well-formed, and the
   * remedy is to change scope rather than to retry. */
  | {
      readonly outcome: 'read_only_scope';
      readonly detail: string;
    }
  | {
      readonly outcome: 'protocol_error';
      readonly authority: string;
      readonly detail: string;
    };

export interface SettingsMutationRequest {
  readonly scope: SettingsMutationScope;
  readonly expectedRevisionId: string;
  readonly idempotencyKey: string;
  readonly readUrl: string;
  readonly patchUrl: string;
  readonly patch: ProjectSettingsChangeSet | UserSettingsChangeSet;
  /** Whether the dashboard scope these routes are addressed in accepts writes.
   * Supplied by the caller rather than read from the store here, so this stays
   * a function of its request and remains directly testable. */
  readonly writability: ScopeWritability;
}

export async function applySettingsMutation(
  request: SettingsMutationRequest,
): Promise<SettingsMutationResult> {
  // Before the refresh, not just before the PATCH. A control the scope has
  // disabled must issue no request at all: re-reading settings the user cannot
  // change is work the daemon was never asked for, and it would make the
  // disabled control look like it had started something.
  const refusal = scopeRefusal(request.writability);
  if (refusal) return refusal;

  const readAuthority = `GET ${request.readUrl}`;
  const current = await fetchJson(request.readUrl);
  if (current.outcome !== 'response') return current;
  if (!current.response.ok) {
    return {
      outcome: 'error',
      detail: `Unable to refresh settings before apply (HTTP ${current.response.status}).`,
    };
  }
  const currentPayload = contractedPayload(current.body, readAuthority);
  if (!currentPayload.parsed) return currentPayload.violation;
  if (!buildSettingsEditor(currentPayload.payload)) {
    return protocolError(
      readAuthority,
      'the response omitted editable values or revision identity.',
    );
  }
  const conflict = settingsRevisionConflict(
    request.scope,
    request.expectedRevisionId,
    currentPayload.payload,
  );
  if (conflict) return { outcome: 'conflict', ...conflict };

  const patchAuthority = `PATCH ${request.patchUrl}`;
  const patched = await fetchJson(request.patchUrl, {
    method: 'PATCH',
    headers: JSON_HEADERS,
    body: JSON.stringify({
      ...request.patch,
      expected_revision_id: request.expectedRevisionId,
      idempotency_key: request.idempotencyKey,
    }),
  });
  if (patched.outcome !== 'response') return patched;
  if (patched.response.status === 409) {
    return readConflict(patched.body, request.expectedRevisionId);
  }
  if (patched.response.status === 503) {
    return { outcome: 'unavailable', detail: unavailableDetail(patched.body) };
  }
  // The gateway's read-only refusal, recognized by its own body rather than by
  // the status alone: a 405 this dashboard cannot account for stays a plain
  // error below rather than borrowing the scope explanation.
  if (patched.response.status === 405) {
    const scopeRefused = readOnlyScopeRefusal(patched.body);
    if (scopeRefused) {
      return {
        outcome: 'read_only_scope',
        detail: `Nothing was applied: ${scopeRefused.detail.replace(/\.$/, '')}.`,
      };
    }
  }
  if (!patched.response.ok) {
    const validation = readValidation(patched.body);
    return validation ?? {
      outcome: 'error',
      detail: `Settings update failed (HTTP ${patched.response.status}).`,
    };
  }
  const projectPatch = request.scope === 'project'
    ? projectSettingsPatchResponse(patched.body, patchAuthority)
    : null;
  if (projectPatch && !projectPatch.parsed) return projectPatch.violation;
  const patchedPayload = contractedPayload(
    projectPatch?.parsed ? projectPatch.current : patched.body,
    patchAuthority,
  );
  if (!patchedPayload.parsed) return patchedPayload.violation;
  const editor = buildSettingsEditor(patchedPayload.payload);
  if (!editor) {
    return protocolError(
      patchAuthority,
      'the response omitted editable values or revision identity.',
    );
  }
  return {
    outcome: 'success',
    scope: request.scope,
    payload: patchedPayload.payload,
    revisionId: settingsRevisionId(editor, request.scope),
    resyncRecommended: patchedPayload.payload.resync_recommended === true,
    restartRecommended: patchedPayload.payload.restart_recommended === true,
    effectReceipt: projectPatch?.parsed ? projectPatch.effectReceipt : null,
  };
}

function projectSettingsPatchResponse(
  body: unknown,
  authority: string,
):
  | {
      readonly parsed: true;
      readonly current: unknown;
      readonly effectReceipt: Readonly<Record<string, unknown>> | null;
    }
  | { readonly parsed: false; readonly violation: Extract<SettingsMutationResult, { outcome: 'protocol_error' }> } {
  const response = z
    .object({
      application_outcome: z.union([
        z.null(),
        z.object({
          Effect: z.object({
            receipt: z.record(z.string(), z.unknown()),
          }).passthrough(),
        }).strict(),
      ]),
      current: z.unknown(),
    })
    .strict()
    .safeParse(body);
  if (!response.success) {
    return {
      parsed: false,
      violation: protocolError(
        authority,
        'the response omitted the canonical application effect receipt or current settings.',
      ),
    };
  }
  return {
    parsed: true,
    current: response.data.current,
    effectReceipt: response.data.application_outcome?.Effect.receipt ?? null,
  };
}

/**
 * The refusal to return instead of writing, or `null` when the scope accepts
 * the write.
 *
 * Exhaustive over `ScopeWritability`, so a state added to the scope authority
 * cannot reach this write as an implicit permission — which is the direction
 * the mistake would go, since anything not matched would fall through to the
 * PATCH.
 */
function scopeRefusal(
  writability: ScopeWritability,
): Extract<SettingsMutationResult, { outcome: 'not_dispatched' }> | null {
  switch (writability.state) {
    case 'writable':
      return null;
    case 'read_only':
    case 'unknown':
      return { outcome: 'not_dispatched', detail: `Nothing was sent. ${writability.reason}` };
    default: {
      const exhaustive: never = writability;
      return exhaustive;
    }
  }
}

type ContractedPayload =
  | { readonly parsed: true; readonly payload: SettingsPayloadV1 }
  | {
      readonly parsed: false;
      readonly violation: Extract<SettingsMutationResult, { outcome: 'protocol_error' }>;
    };

/**
 * The settings payload as the generated contract accepts it, or the named
 * authority that failed to produce one. The two refusals are different
 * accusations and stay apart: a body that is not the envelope at all, and an
 * envelope whose payload the contract rejects.
 */
function contractedPayload(body: unknown, authority: string): ContractedPayload {
  const read = readSettingsEnvelope(body);
  switch (read.outcome) {
    case 'settings':
      return { parsed: true, payload: read.payload };
    case 'not_an_envelope':
      return {
        parsed: false,
        violation: protocolError(authority, 'expected an envelope carrying a payload.'),
      };
    case 'unsupported_payload':
      return {
        parsed: false,
        violation: protocolError(
          authority,
          'the response omitted editable values or revision identity.',
        ),
      };
    default: {
      const exhaustive: never = read;
      return exhaustive;
    }
  }
}

/**
 * CONTRACT GAP — the settings refusal bodies are not generated.
 *
 * `/api/settings/{project,user}` build their 400/409/503 bodies with
 * `serde_json::json!` in `crates/tracedecay-api/src/configuration.rs`
 * (`settings_validation_error`, `configuration_revision_conflict_error`,
 * `configuration_authority_unavailable_error`), so schemars never sees them
 * and `contracts/generated.ts` carries no type for them. The success payload
 * is contracted and is parsed as such above; only the refusals are read here.
 *
 * Every member below is therefore optional and every absence resolves to a
 * stated unavailable value — a missing `actual_revision_id` reads as unknown,
 * a missing `detail` says the authority is unavailable without naming which.
 * None of it is inferred, and none of it turns a refusal into a success.
 */
const RefusalBodySchema = z
  .object({
    detail: z.string().optional(),
    expected_revision_id: z.string().optional(),
    actual_revision_id: z.string().optional(),
    validation_errors: z
      .array(z.object({ field: z.string(), message: z.string() }).passthrough())
      .optional(),
  })
  .passthrough();

function readRefusal(body: unknown): z.infer<typeof RefusalBodySchema> {
  const parsed = RefusalBodySchema.safeParse(body);
  return parsed.success ? parsed.data : {};
}

function unavailableDetail(body: unknown): string {
  const detail = readRefusal(body).detail;
  return detail != null && detail.length > 0
    ? `Nothing was applied: ${detail.replace(/\.$/, '')}.`
    : 'Nothing was applied: the authority for this write is unavailable.';
}

type JsonFetchResult =
  | { readonly outcome: 'response'; readonly response: Response; readonly body: unknown }
  | {
      readonly outcome: 'offline';
      readonly detail: string;
    }
  | Extract<SettingsMutationResult, { outcome: 'protocol_error' }>;

async function fetchJson(url: string, init?: RequestInit): Promise<JsonFetchResult> {
  const authority = `${init?.method ?? 'GET'} ${url}`;
  let response: Response;
  try {
    response = await fetch(url, {
      headers: { accept: 'application/json' },
      ...init,
    });
  } catch {
    return { outcome: 'offline', detail: 'The daemon is unreachable.' };
  }
  try {
    return { outcome: 'response', response, body: await response.json() };
  } catch {
    return protocolError(authority, 'expected JSON.');
  }
}

function protocolError(
  authority: string,
  reason: string,
): Extract<SettingsMutationResult, { outcome: 'protocol_error' }> {
  return {
    outcome: 'protocol_error',
    authority,
    detail: `${authority} violated the settings contract: ${reason}`,
  };
}

function readValidation(body: unknown): SettingsMutationResult | null {
  const refusal = readRefusal(body);
  const errors: readonly SettingsValidationError[] = (refusal.validation_errors ?? []).map(
    ({ field, message }) => ({ field, message }),
  );
  if (errors.length === 0) return null;
  return {
    outcome: 'validation',
    detail: refusal.detail ?? 'Settings validation failed.',
    errors,
  };
}

function readConflict(
  body: unknown,
  fallbackExpectedRevisionId: string,
): SettingsMutationResult {
  const refusal = readRefusal(body);
  return {
    outcome: 'conflict',
    expectedRevisionId: refusal.expected_revision_id ?? fallbackExpectedRevisionId,
    actualRevisionId: refusal.actual_revision_id ?? null,
  };
}

const JSON_HEADERS = {
  accept: 'application/json',
  'content-type': 'application/json',
} as const;
