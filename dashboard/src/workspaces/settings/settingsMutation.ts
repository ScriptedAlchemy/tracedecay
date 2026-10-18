import { dashboardFetch } from '../../data/auth/credential.ts';
import {
  DashboardConfigurationRefusalV1Schema,
  ProjectSettingsPatchResponseV1Schema,
  type EffectReceipt,
  type SettingsPayloadV1,
} from '../../contracts/generated.ts';
import { applicationProblemStatusMatches } from '../../contracts/application.ts';
import { readOnlyScopeRefusal, type ScopeWritability } from '../../data/scope/store.ts';
import {
  buildSettingsEditor,
  readSettingsEnvelope,
  settingsRevisionConflict,
  settingsRevisionId,
  type CodeIndexWorkerSettingsChangeSet,
  type ProjectSettingsChangeSet,
  type SettingsScope,
  type SettingsValidationError,
  type UserSettingsChangeSet,
} from './settingsModel.ts';

type SettingsMutationScope = SettingsScope;

export type SettingsMutationResult =
  | {
      readonly outcome: 'success';
      readonly scope: SettingsMutationScope;
      readonly payload: SettingsPayloadV1;
      readonly revisionId: string;
      readonly resyncRecommended: boolean;
      readonly restartRecommended: boolean;
      /** Canonical durable application receipt for project writes. */
      readonly effectReceipt: EffectReceipt | null;
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
  | {
      readonly outcome: 'unsettled';
      readonly detail: string;
    }
  /** The authority that performs this write is not currently mounted. Kept
   * apart from `error`: nothing was attempted, and nothing changed. */
  | {
      readonly outcome: 'unavailable';
      readonly detail: string;
    }
  /** The scope declined the write before anything was sent. Not a failure of
   * the write, the absence of one, with the scope authority's own reason. */
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
  readonly patch:
    | ProjectSettingsChangeSet
    | UserSettingsChangeSet
    | CodeIndexWorkerSettingsChangeSet;
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
    return { outcome: 'error', detail: 'Settings update failed (HTTP 405).' };
  }
  if (!patched.response.ok) {
    return settingsRefusal(patched.body, patched.response.status, patchAuthority);
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
      readonly effectReceipt: EffectReceipt | null;
    }
  | { readonly parsed: false; readonly violation: Extract<SettingsMutationResult, { outcome: 'protocol_error' | 'unsettled' }> } {
  const response = ProjectSettingsPatchResponseV1Schema.safeParse(body);
  if (!response.success) {
    return {
      parsed: false,
      violation: protocolError(
        authority,
        'the response omitted the canonical application effect receipt or current settings.',
      ),
    };
  }
  const outcome = response.data.application_outcome;
  if (outcome !== null && (
    outcome.outcome !== 'effect'
    || outcome.value.execution.termination !== 'completed'
    || outcome.value.reconciliation !== 'reconciled'
    || outcome.value.receipt.outcome !== 'completed'
    || outcome.value.receipt.committed_state === null
  )) {
    return {
      parsed: false,
      violation: { outcome: 'unsettled', detail: 'The settings effect is not confirmed. Refresh current values before retrying.' },
    };
  }
  return {
    parsed: true,
    current: response.data.current,
    effectReceipt: outcome?.outcome === 'effect' ? outcome.value.receipt : null,
  };
}

/**
 * The refusal to return instead of writing, or `null` when the scope accepts
 * the write.
 *
 * Exhaustive over `ScopeWritability`, so a state added to the scope authority
 * cannot reach this write as an implicit permission, which is the direction
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

function settingsRefusal(body: unknown, status: number, authority: string): SettingsMutationResult {
  const parsed = DashboardConfigurationRefusalV1Schema.safeParse(body);
  if (!parsed.success) return protocolError(authority, 'expected a configuration refusal.');
  const refusal = parsed.data;
  if ('problem' in refusal) {
    // The existing settings adapter preserves HTTP 409 for cancellation.
    const matches = refusal.problem.kind === 'cancelled'
      ? status === 409
      : applicationProblemStatusMatches(status, refusal.problem.kind);
    if (!matches) return protocolError(authority, 'the application problem disagrees with its HTTP status.');
    if (refusal.problem.committed_receipt !== null) {
      return { outcome: 'unsettled', detail: `${refusal.problem.message} Refresh current values before retrying.` };
    }
    return { outcome: 'error', detail: refusal.problem.message };
  }
  if ('validation_errors' in refusal) {
    return status === 400
      ? { outcome: 'validation', detail: refusal.detail, errors: refusal.validation_errors }
      : protocolError(authority, 'the validation refusal disagrees with its HTTP status.');
  }
  switch (refusal.code) {
    case 'configuration_revision_conflict':
      return status === 409
        ? { outcome: 'conflict', expectedRevisionId: refusal.expected_revision_id, actualRevisionId: refusal.actual_revision_id }
        : protocolError(authority, 'the revision conflict disagrees with its HTTP status.');
    case 'configuration_authority_unavailable':
      return status === 503
        ? { outcome: 'unavailable', detail: `Nothing was applied: ${refusal.detail.replace(/\.$/, '')}.` }
        : protocolError(authority, 'the authority refusal disagrees with its HTTP status.');
    case 'application_contract_violation':
      return protocolError(authority, refusal.detail);
  }
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
    response = await dashboardFetch(url, {
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

const JSON_HEADERS = {
  accept: 'application/json',
  'content-type': 'application/json',
} as const;
