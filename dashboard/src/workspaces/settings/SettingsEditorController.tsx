/**
 * The controller: one state machine, and the one place a settings write is
 * issued from.
 *
 * It holds no state of its own beyond the machine. The authority is whatever
 * the last parsed read produced, the draft and the review live in the machine,
 * and a request exists only where the machine has reached `submitting` — which
 * it only does after re-deriving the confirmed change against the revision the
 * editor currently holds. There is no path from a click straight to a PATCH.
 */

import { useMutation } from '@tanstack/react-query';
import { useEffect, useMemo, useReducer, useRef } from 'react';
import type { SettingsPayloadV1 } from '../../contracts/generated.ts';
import { mintBrowserIdempotencyKey } from '../../data/identity.ts';
import { ProjectSettingsFields, UserSettingsFields } from './SettingsFields.tsx';
import { SettingsReviewDialog } from './SettingsReviewDialog.tsx';
import {
  initialSettingsEditorState,
  reduceSettingsEditor,
  settingsApplied,
  settingsFieldErrors,
  settingsRejection,
  settingsScopeDirty,
  settingsSubmission,
  type SettingsRoutes,
} from './settingsEditorMachine.ts';
import { buildSettingsEditor } from './settingsModel.ts';
import { applySettingsMutation } from './settingsMutation.ts';
import type { ScopeWritability } from '../../data/scope/store.ts';

/**
 * Whether a settings scope may be written, and why not when it may not.
 *
 * Two independent authorities have to agree, and a boolean could only report
 * their conjunction — which left a disabled editor claiming "this dashboard is
 * not authorized" when the real obstacle was that the selected project is not
 * the active one. They stay distinguishable:
 *
 *   - `unauthorized`: the envelope advertises no apply action for this scope,
 *     so the daemon has no mounted authority for it.
 *   - `read_only` / `unknown`: the scope this dashboard is pointed at, from
 *     `scopeWritable`.
 */
export type SettingsWriteGate =
  | { readonly state: 'writable'; readonly target: string }
  | { readonly state: 'unauthorized' }
  | { readonly state: 'read_only'; readonly reason: string }
  | { readonly state: 'unknown'; readonly reason: string };

export interface WritableScopes {
  readonly project: SettingsWriteGate;
  readonly user: SettingsWriteGate;
}

/**
 * Fold the two authorities into one gate.
 *
 * Server authorization is checked first: without an advertised apply action
 * there is nothing to write in any scope, so naming the scope would point at
 * the wrong obstacle. Exhaustive over `ScopeWritability`.
 */
export function settingsWriteGate(
  authorized: boolean,
  writability: ScopeWritability,
): SettingsWriteGate {
  if (!authorized) return { state: 'unauthorized' };
  switch (writability.state) {
    case 'writable':
      return { state: 'writable', target: writability.target };
    case 'read_only':
      return { state: 'read_only', reason: writability.reason };
    case 'unknown':
      return { state: 'unknown', reason: writability.reason };
    default: {
      const exhaustive: never = writability;
      return exhaustive;
    }
  }
}

export function SettingsEditorPanel({
  payload,
  writable,
  writability,
  readUrl,
  projectPatchUrl,
  userPatchUrl,
  onApplied,
}: {
  payload: SettingsPayloadV1;
  writable: WritableScopes;
  writability: ScopeWritability;
  readUrl: string;
  projectPatchUrl: string;
  userPatchUrl: string;
  onApplied: () => void;
}) {
  const authority = useMemo(() => buildSettingsEditor(payload), [payload]);
  const [state, dispatch] = useReducer(
    reduceSettingsEditor,
    authority,
    initialSettingsEditorState,
  );
  const routes = useMemo<SettingsRoutes>(
    () => ({ readUrl, projectPatchUrl, userPatchUrl }),
    [readUrl, projectPatchUrl, userPatchUrl],
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
    // The scope authority travels with the request, so what disabled the field
    // set and what the write refuses on are the same reading rather than two
    // that could be taken at different moments.
    mutate({ ...settingsSubmission(state, routes), writability });
  }, [state, routes, mutate, writability]);

  if (state.status === 'editor_unavailable') {
    return (
      <section className="border-b border-edge-subtle p-3" aria-label="Supported settings changes">
        <p className="text-xs text-state-error">
          Settings editing requires project configuration values and configuration_revision_id
          from GET /api/settings, plus user settings and configuration_revision_id from the same
          authority. The response omitted at least one required field.
        </p>
      </section>
    );
  }

  const applied = settingsApplied(state);
  const rejection = settingsRejection(state);
  const errors = settingsFieldErrors(state);

  return (
    <section
      className="border-b border-edge-subtle bg-surface-0 p-3"
      aria-labelledby="settings-editor-title"
    >
      <div className="mb-3 flex flex-wrap items-baseline gap-2">
        <h2 id="settings-editor-title" className="td-title">
          Supported settings changes
        </h2>
        <span className="text-2xs text-text-muted">
          validate → review → confirm against the resource revision
        </span>
      </div>

      {applied ? (
        <div
          role="status"
          className="mb-3 flex flex-wrap gap-2 border border-state-ready/40 bg-surface-1 px-3 py-2 text-xs text-text-secondary"
        >
          <strong className="font-semibold text-text-primary">{applied.message}</strong>
          {applied.resyncRecommended ? <span>Resync recommended</span> : null}
          {applied.restartRecommended ? <span>Restart recommended</span> : null}
        </div>
      ) : null}

      {rejection?.origin === 'server' ? (
        // A refusal by the write authority is not the same statement as a
        // value this form declined to send, so the surface names which one it
        // is rather than leaving both as red text beside a field.
        <p role="status" className="mb-3 text-xs text-state-error">
          The daemon rejected this {rejection.scope} settings change: {rejection.detail}
        </p>
      ) : null}

      <div className="grid gap-3 xl:grid-cols-2">
        <ProjectSettingsFields
          values={state.draft.project}
          errors={errors}
          dirty={settingsScopeDirty(state, 'project')}
          writable={writable.project}
          onChange={(values) => dispatch({ type: 'project_drafted', values })}
          onReview={() =>
            dispatch({
              type: 'review_requested',
              scope: 'project',
              idempotencyKey: mintBrowserIdempotencyKey('dashboard-settings'),
            })
          }
        />
        <UserSettingsFields
          values={state.draft.user}
          errors={errors}
          dirty={settingsScopeDirty(state, 'user')}
          writable={writable.user}
          onChange={(values) => dispatch({ type: 'user_drafted', values })}
          onReview={() =>
            dispatch({
              type: 'review_requested',
              scope: 'user',
              idempotencyKey: mintBrowserIdempotencyKey('dashboard-settings'),
            })
          }
        />
      </div>

      <SettingsReviewDialog
        state={state}
        onConfirmedChange={(confirmed) => dispatch({ type: 'confirmation_set', confirmed })}
        onDismiss={() => dispatch({ type: 'review_dismissed' })}
        onApply={() => dispatch({ type: 'submit_started' })}
        onReload={() => {
          dispatch({ type: 'reloaded_from_authority' });
          onApplied();
        }}
      />
    </section>
  );
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
