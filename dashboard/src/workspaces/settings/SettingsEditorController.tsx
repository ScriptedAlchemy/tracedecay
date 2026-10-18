/**
 * The controller: one state machine, and the one place a settings write is
 * issued from.
 *
 * It holds no state of its own beyond the machine. The authority is whatever
 * the last parsed read produced, the draft and the review live in the machine,
 * and a request exists only where the machine has reached `submitting`, which
 * it only does after re-deriving the confirmed change against the revision the
 * editor currently holds. There is no path from a click straight to a PATCH.
 *
 * It is a hook rather than a panel because the effective-configuration table,
 * the inline review, and the inspector all read the same machine: the row that
 * says `edited`, the panel that shows the frozen patch, and the register that
 * says `review pending` are three views of one state.
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
  /** Replace one bound field in the draft. Drops any open review, as the machine does. */
  readonly edit: (binding: SettingsBinding, value: unknown) => void;
  /** Take the authority's value back for one bound field, leaving other edits alone. */
  readonly revert: (binding: SettingsBinding) => void;
  /** Freeze this scope's change against the held revision. */
  readonly review: (scope: SettingsScope) => void;
  readonly setConfirmed: (confirmed: boolean) => void;
  readonly apply: () => void;
  readonly dismiss: () => void;
  /** Discard every draft and take the authority's current values. */
  readonly reload: () => void;
}

export function useSettingsEditor({
  payload,
  routes,
  writability,
  onApplied,
}: {
  payload: SettingsPayloadV1;
  routes: SettingsRoutes;
  writability: ScopeWritability;
  onApplied: () => void;
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
    onSuccess: (result) => {
      dispatch({ type: 'submit_settled', result });
      if (result.outcome === 'success') onApplied();
    },
    onError: (error) => {
      dispatch({
        type: 'submit_settled',
        result: { outcome: 'error', detail: unreportedFailureDetail(error) },
      });
    },
  });

  // The request is derived from the submitting state, so it cannot be built
  // from a draft the machine has not checked. The ref keeps one entry into
  // `submitting` to one PATCH; leaving the state releases it for a retry.
  const inFlight = useRef<string | null>(null);
  useEffect(() => {
    if (state.status !== 'submitting') {
      inFlight.current = null;
      return;
    }
    if (inFlight.current === state.review.reviewId) return;
    inFlight.current = state.review.reviewId;
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

  const review = useCallback((scope: SettingsScope) => {
    dispatch({
      type: 'review_requested',
      scope,
      idempotencyKey: mintBrowserIdempotencyKey('dashboard-settings'),
    });
  }, []);

  const setConfirmed = useCallback((confirmed: boolean) => {
    dispatch({ type: 'confirmation_set', confirmed });
  }, []);
  const apply = useCallback(() => dispatch({ type: 'submit_started' }), []);
  const dismiss = useCallback(() => dispatch({ type: 'review_dismissed' }), []);
  const reload = useCallback(() => {
    dispatch({ type: 'reloaded_from_authority' });
    onApplied();
  }, [onApplied]);

  return useMemo(
    () => ({ state, edit, revert, review, setConfirmed, apply, dismiss, reload }),
    [state, edit, revert, review, setConfirmed, apply, dismiss, reload],
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
