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
      readonly outcome: 'offline' | 'error';
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
    return protocolError(`GET ${request.readUrl}`, 'expected a JSON object.');
  }
  if (!buildSettingsEditor(current.body)) {
    return protocolError(
      `GET ${request.readUrl}`,
      'the response omitted editable values or revision identity.',
    );
  }
  const conflict = settingsRevisionConflict(
    request.scope,
    request.expectedRevisionId,
    current.body,
  );
  if (conflict) return { outcome: 'conflict', ...conflict };

  const patched = await fetchJson(request.patchUrl, {
    method: 'PATCH',
    headers: JSON_HEADERS,
    body: JSON.stringify({
      ...request.patch,
      expected_revision_id: request.expectedRevisionId,
    }),
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
    return protocolError(`PATCH ${request.patchUrl}`, 'expected a JSON object.');
  }
  const editor = buildSettingsEditor(patched.body);
  if (!editor) {
    return protocolError(
      `PATCH ${request.patchUrl}`,
      'the response omitted editable values or revision identity.',
    );
  }
  return {
    outcome: 'success',
    scope: request.scope,
    payload: patched.body,
    revisionId:
      request.scope === 'project'
        ? editor.projectExpectedRevisionId
        : editor.userExpectedRevisionId,
    resyncRecommended: patched.body['resync_recommended'] === true,
    restartRecommended: patched.body['restart_recommended'] === true,
  };
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
