/**
 * The review surface: the exact patch about to be sent, the revision it is
 * held against, the confirmation, and whatever verdict came back.
 *
 * It renders one state of the editor machine and reports intent back. It never
 * decides whether a change may be applied — the machine does — so a state that
 * cannot submit simply offers no way to try.
 */

import * as Dialog from '@radix-ui/react-dialog';
import { X } from 'lucide-react';
import { secondarySettingsButtonClass, settingsButtonClass } from './settingsChrome.ts';
import {
  settingsConfirmationHeld,
  settingsReviewOf,
  type SettingsEditorState,
} from './settingsEditorMachine.ts';

export function SettingsReviewDialog({
  state,
  onConfirmedChange,
  onDismiss,
  onApply,
  onReload,
}: {
  state: SettingsEditorState;
  onConfirmedChange: (confirmed: boolean) => void;
  onDismiss: () => void;
  onApply: () => void;
  onReload: () => void;
}) {
  const review = settingsReviewOf(state);
  const scope = review?.scope ?? 'project';
  const confirmed = settingsConfirmationHeld(state);
  const applying = state.status === 'submitting';
  const resolvable = state.status === 'conflicted' || state.status === 'review_superseded';
  return (
    <Dialog.Root open={review != null} onOpenChange={(open) => (open ? undefined : onDismiss())}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/60" />
        <Dialog.Content className="fixed left-1/2 top-1/2 z-50 max-h-[calc(100dvh-2rem)] w-[min(36rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 p-5 shadow-xl">
          <div className="flex items-start justify-between gap-3">
            <div>
              <Dialog.Title className="text-base font-semibold tracking-tight">
                Review {scope} settings change
              </Dialog.Title>
              <Dialog.Description className="mt-1 text-xs text-text-muted">
                Only the validated changed fields below will be sent. The held revision is checked
                again immediately before apply.
              </Dialog.Description>
            </div>
            <Dialog.Close
              aria-label="Close settings review"
              // The glyph stays 16px — it is a dismiss, not an action the
              // dialog is about — and the element around it carries the touch
              // minimum. See `.td-hit`. The negative margin lets the 44px box
              // use the dialog's own padding instead of adding a row of height
              // to the header.
              className="td-hit group -mr-2 -mt-2 shrink-0"
            >
              <span className="inline-flex size-6 items-center justify-center rounded-[var(--radius-chip)] text-text-muted group-hover:bg-surface-2 group-hover:text-text-primary">
                <X aria-hidden size={16} />
              </span>
            </Dialog.Close>
          </div>
          {review ? (
            <div className="mt-4 grid gap-3">
              <pre className="max-h-56 overflow-auto border border-edge-subtle bg-surface-0 p-3 text-2xs text-text-secondary">
                {JSON.stringify(review.patch, null, 2)}
              </pre>
              <p className="break-all font-mono text-2xs text-text-muted">
                expected revision {review.expectedRevisionId}
              </p>
              <label className="flex cursor-pointer items-center gap-1 border border-edge-subtle py-2 pr-3 text-xs text-text-secondary">
                <input
                  type="checkbox"
                  className="td-check"
                  checked={confirmed}
                  onChange={(event) => onConfirmedChange(event.target.checked)}
                />
                <span className="min-w-0">
                  I confirm this change against configuration revision{' '}
                  {review.expectedRevisionId}.
                </span>
              </label>
              <ReviewVerdict state={state} />
              <div className="flex justify-end gap-2">
                <Dialog.Close className={secondarySettingsButtonClass}>Cancel</Dialog.Close>
                {resolvable ? (
                  <button type="button" className={settingsButtonClass} onClick={onReload}>
                    Load current values
                  </button>
                ) : (
                  <button
                    type="button"
                    className={settingsButtonClass}
                    disabled={!confirmed || applying}
                    onClick={onApply}
                  >
                    {applying ? `Applying ${scope} settings` : `Apply ${scope} settings`}
                  </button>
                )}
              </div>
            </div>
          ) : null}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

/**
 * What came back, said as what it is. A revision the authority refused, a
 * revision that moved before anything was sent, an authority that is not
 * mounted, and a write that never reached a verdict are four different
 * statements, and none of them is "the save failed".
 */
function ReviewVerdict({ state }: { state: SettingsEditorState }) {
  switch (state.status) {
    case 'conflicted':
      return (
        <p role="alert" className="text-xs text-state-conflicting">
          Another writer saved {state.review.scope} settings after this form loaded. Your draft was
          based on {state.conflict.expectedRevisionId}; the current authority is{' '}
          {state.conflict.actualRevisionId ?? 'unknown'}. Nothing was applied.
        </p>
      );
    case 'review_superseded':
      return (
        <p role="alert" className="text-xs text-state-conflicting">
          The {state.review.scope} settings authority moved from{' '}
          {state.review.expectedRevisionId} to {state.currentRevisionId} while this change was
          under review, so this change no longer describes it. Nothing was sent.
        </p>
      );
    case 'authority_withdrawn':
      return (
        <p role="alert" className="text-xs text-state-unsupported-schema">
          {state.detail}
        </p>
      );
    case 'submit_failed':
      // Every failure kind states its own reason, including which authority
      // broke the contract; there is nothing to add to it here.
      return (
        <p role="alert" className="text-xs text-state-error">
          {state.failure.detail}
        </p>
      );
    case 'editor_unavailable':
    case 'editing':
    case 'reviewing':
    case 'confirmed':
    case 'submitting':
      return null;
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}