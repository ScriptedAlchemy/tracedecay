import {
  buildSettingsEditor,
  settingsRevisionConflict,
  type ProjectSettingsPatch,
  type SettingsValidationError,
  type UserSettingsPatch,
} from './settingsModel.ts';

export type SettingsMutationScope = 'project' | 'user';

export type SettingsMutationResult =
  | {
      readonly outcome: 'success';
      readonly scope: SettingsMutationScope;
      readonly payload: Record<string, unknown>;
      readonly revisionId: string;
      readonly resyncRecommended: boolean;
      readonly restartRecommended: boolean;
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
      readonly outcome: 'offline' | 'error' | 'unsupported_schema';
      readonly detail: string;
    };

export interface SettingsMutationRequest {
  readonly scope: SettingsMutationScope;
  readonly expectedRevisionId: string;
  readonly readUrl: string;
  readonly patchUrl: string;
  readonly patch: ProjectSettingsPatch | UserSettingsPatch;
}

export async function applySettingsMutation(
  request: SettingsMutationRequest,
): Promise<SettingsMutationResult> {
  const current = await fetchJson(request.readUrl);
  if (current.outcome !== 'response') return current;
  if (!current.response.ok) {
    return {
      outcome: 'error',
      detail: `Unable to refresh settings before apply (HTTP ${current.response.status}).`,
    };
  }
  if (!isRecord(current.body)) {
    return {
      outcome: 'unsupported_schema',
      detail: 'The current settings response could not be decoded.',
    };
  }
  const conflict = settingsRevisionConflict(request.expectedRevisionId, current.body);
  if (conflict) return { outcome: 'conflict', ...conflict };

  const patched = await fetchJson(request.patchUrl, {
    method: 'PATCH',
    headers: JSON_HEADERS,
    body: JSON.stringify(request.patch),
  });
  if (patched.outcome !== 'response') return patched;
  if (patched.response.status === 409) {
    return readConflict(patched.body, request.expectedRevisionId);
  }
  if (!patched.response.ok) {
    const validation = readValidation(patched.body);
    return validation ?? {
      outcome: 'error',
      detail: `Settings update failed (HTTP ${patched.response.status}).`,
    };
  }
  if (!isRecord(patched.body)) {
    return {
      outcome: 'unsupported_schema',
      detail: 'The updated settings response could not be decoded.',
    };
  }
  const editor = buildSettingsEditor(patched.body);
  if (!editor) {
    return {
      outcome: 'unsupported_schema',
      detail: 'The updated settings response omitted editable values or revision identity.',
    };
  }
  return {
    outcome: 'success',
    scope: request.scope,
    payload: patched.body,
    revisionId: editor.expectedRevisionId,
    resyncRecommended: patched.body['resync_recommended'] === true,
    restartRecommended: patched.body['restart_recommended'] === true,
  };
}

type JsonFetchResult =
  | { readonly outcome: 'response'; readonly response: Response; readonly body: unknown }
  | {
      readonly outcome: 'offline' | 'unsupported_schema';
      readonly detail: string;
    };

async function fetchJson(url: string, init?: RequestInit): Promise<JsonFetchResult> {
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
    return {
      outcome: 'unsupported_schema',
      detail: 'The daemon returned a response that was not JSON.',
    };
  }
}

function readValidation(body: unknown): SettingsMutationResult | null {
  if (!isRecord(body) || !Array.isArray(body['validation_errors'])) return null;
  const errors = body['validation_errors'].flatMap((item): SettingsValidationError[] => {
    if (!isRecord(item)) return [];
    const field = item['field'];
    const message = item['message'];
    return typeof field === 'string' && typeof message === 'string'
      ? [{ field, message }]
      : [];
  });
  if (errors.length === 0) return null;
  return {
    outcome: 'validation',
    detail:
      typeof body['detail'] === 'string' ? body['detail'] : 'Settings validation failed.',
    errors,
  };
}

function readConflict(
  body: unknown,
  fallbackExpectedRevisionId: string,
): SettingsMutationResult {
  if (!isRecord(body)) {
    return {
      outcome: 'conflict',
      expectedRevisionId: fallbackExpectedRevisionId,
      actualRevisionId: null,
    };
  }
  return {
    outcome: 'conflict',
    expectedRevisionId:
      typeof body['expected_revision_id'] === 'string'
        ? body['expected_revision_id']
        : fallbackExpectedRevisionId,
    actualRevisionId:
      typeof body['actual_revision_id'] === 'string' ? body['actual_revision_id'] : null,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

const JSON_HEADERS = {
  accept: 'application/json',
  'content-type': 'application/json',
} as const;
