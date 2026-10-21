/**
 * The controller: one state machine, and the one place a settings write is
 * issued from.
 *
 * It holds no state of its own beyond the machine. Each resource's authority
 * comes from a parsed read or verified PATCH, the draft lives in the machine,
 * and Save captures one patch against that revision before any request is issued.
 *
 * It is a hook rather than a panel because the table, inline editor, and
 * inspector all read the same machine.
 */

import { useMutation } from '@tanstack/react-query';
import { useCallback, useEffect, useMemo, useReducer, useRef } from 'react';
import type { SettingsPayloadV1 } from '../../contracts/generated.ts';
import { mintBrowserIdempotencyKey } from '../../data/identity.ts';
import type { ScopeWritability } from '../../data/scope/store.ts';
import {
  initialSettingsEditorState,
  reduceSettingsEditor,
  settingsSubmission,
  type SettingsEditorAction,
  type SettingsEditorState,
  type SettingsRoutes,
} from './settingsEditorMachine.ts';
import { PROFILE_WORKER_WRITABILITY } from './settingsGates.ts';
import { buildSettingsEditor, type SettingsScope } from './settingsModel.ts';
import { applySettingsMutation } from './settingsMutation.ts';
import { authorityValue, withFieldValue, type SettingsBinding } from './settingsRows.ts';

export interface SettingsEditorHandle {
  readonly state: SettingsEditorState;
  /** Replace one bound field in the draft. */
  readonly edit: (binding: SettingsBinding, value: unknown) => void;
  /** Take the authority's value back for one bound field, leaving other edits alone. */
  readonly revert: (binding: SettingsBinding) => void;
  /** Save this scope's current draft against its held revision. */
  readonly save: (scope: SettingsScope) => void;
  /** Retry the same immutable write with its original idempotency key. */
  readonly retry: () => void;
  /** Discard every draft and take the authority's current values. */
  readonly reload: () => void;
}

export function useSettingsEditor({
  payload,
  routes,
  writability,
  refresh,
  publishApplied,
}: {
  payload: SettingsPayloadV1;
  routes: SettingsRoutes;
  writability: ScopeWritability;
  refresh: () => Promise<SettingsPayloadV1 | null>;
  publishApplied: (scope: SettingsScope, payload: SettingsPayloadV1) => Promise<void>;
}): SettingsEditorHandle {
  const authority = useMemo(() => buildSettingsEditor(payload), [payload]);
  const [state, dispatch] = useReducer(
    reduceSettingsEditor,
    authority,
    initialSettingsEditorState,
  );

  useEffect(() => {
    dispatch({ type: 'authority_observed', authority });
  }, [authority]);

  const { mutate } = useMutation({
    mutationFn: applySettingsMutation,
    onSuccess: async (result) => {
      // The table reads the canonical query snapshot. Publish the verified
      // saved resource before clearing its proposal, even if readback stalls.
      if (result.outcome === 'success') await publishApplied(result.scope, result.payload);
      dispatch({ type: 'submit_settled', result });
      if (result.outcome === 'success') void refresh();
    },
    onError: (error) => {
      dispatch({
        type: 'submit_settled',
        result: { outcome: 'error', detail: unreportedFailureDetail(error) },
      });
    },
  });

  // One transition into `submitting` can issue one request. Leaving that state
  // releases the guard for a retry with the same idempotency key.
  const inFlight = useRef<string | null>(null);
  useEffect(() => {
    if (state.status !== 'submitting') {
      inFlight.current = null;
      return;
    }
    if (inFlight.current === state.submission.idempotencyKey) return;
    inFlight.current = state.submission.idempotencyKey;
    // Project and ordinary user writes use the current project gateway reading.
    // The ProfileSessions worker preference is profile-global, so its request
    // never inherits selected-project writability.
    const submission = settingsSubmission(state, routes);
    mutate({
      ...submission,
      writability:
        submission.scope === 'code_index_workers' ? PROFILE_WORKER_WRITABILITY : writability,
    });
  }, [state, routes, mutate, writability]);

  const edit = useCallback(
    (binding: SettingsBinding, value: unknown) => {
      if (state.status === 'editor_unavailable') return;
      dispatch(draftAction(binding, withFieldValue(state.draft, binding, value)));
    },
    [state],
  );

  const revert = useCallback(
    (binding: SettingsBinding) => {
      if (state.status === 'editor_unavailable') return;
      dispatch(
        draftAction(
          binding,
          withFieldValue(state.draft, binding, authorityValue(state.authority, binding)),
        ),
      );
    },
    [state],
  );

  const save = useCallback((scope: SettingsScope) => {
    dispatch({
      type: 'save_requested',
      scope,
      idempotencyKey: mintBrowserIdempotencyKey('dashboard-settings'),
    });
  }, []);

  const retry = useCallback(() => dispatch({ type: 'retry_requested' }), []);
  const reloadPending = useRef(false);
  const reload = useCallback(async () => {
    if (reloadPending.current) return;
    reloadPending.current = true;
    try {
      const current = await refresh();
      const reloadedAuthority = current === null ? null : buildSettingsEditor(current);
      // A failed or pending read cannot settle an uncertain write. Carry the
      // successful read explicitly: React Query may reuse a snapshot already
      // observed while the draft was dirty, so its effect need not run again.
      if (reloadedAuthority !== null) {
        dispatch({ type: 'reloaded_from_authority', authority: reloadedAuthority });
      }
    } finally {
      reloadPending.current = false;
    }
  }, [refresh]);

  return useMemo(
    () => ({ state, edit, revert, save, retry, reload }),
    [state, edit, revert, save, retry, reload],
  );
}

/** The one draft action a binding's scope answers to. */
function draftAction(
  binding: SettingsBinding,
  draft: ReturnType<typeof withFieldValue>,
): SettingsEditorAction {
  switch (binding.scope) {
    case 'project':
      return { type: 'project_drafted', values: draft.project };
    case 'user':
      return { type: 'user_drafted', values: draft.user };
    case 'code_index_workers':
      return { type: 'code_index_workers_drafted', values: draft.codeIndexWorkers };
    default: {
      const exhaustive: never = binding;
      return exhaustive;
    }
  }
}

/** `applySettingsMutation` answers every failure it can name; anything that
 * still throws has no reason on the wire, and the surface says exactly that
 * rather than assigning one. */
function unreportedFailureDetail(error: unknown): string {
  const message = error instanceof Error ? error.message.trim() : '';
  return message.length > 0
    ? `The settings write did not complete: ${message}`
    : 'The settings write did not complete and reported no reason.';
}
