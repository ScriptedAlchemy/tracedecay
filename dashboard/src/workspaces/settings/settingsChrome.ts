/* Form controls, not instrument chrome: these edit and commit configuration,
 * so they take the touch minimum on their own box rather than hiding a compact
 * bezel inside a larger hit area the way the panel-header controls do. `+2px`
 * on the input is its two hairlines, so the content box lands on 44.
 *
 * Shared by the editor fields and the review dialog, which are one control
 * surface split across two modules. */

export const settingsInputClass =
  'h-[calc(var(--touch-target-min)+2px)] w-full rounded-[var(--radius-chip)] border border-edge-subtle bg-surface-0 px-2 text-xs text-text-primary outline-none focus-visible:border-accent';

export const settingsButtonClass =
  'inline-flex min-h-[var(--touch-target-min)] items-center justify-center rounded-[var(--radius-standard)] border border-accent/50 bg-accent/15 px-3 text-2xs font-semibold text-text-primary hover:border-accent disabled:cursor-not-allowed disabled:opacity-50';

export const secondarySettingsButtonClass =
  'inline-flex min-h-[var(--touch-target-min)] items-center justify-center rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 px-3 text-2xs font-medium text-text-secondary hover:text-text-primary';

/* The same bezel as `settingsInputClass`, so a boolean field reads as a field
 * rather than as loose text beside the inputs it sits among. The row carries no
 * left padding of its own: `.td-check` is a 44px box around a 16px bezel, and
 * that inset IS the padding. */
export const settingsCheckboxRowClass =
  'flex min-h-[calc(var(--touch-target-min)+2px)] w-full cursor-pointer items-center rounded-[var(--radius-chip)] border border-edge-subtle bg-surface-0 text-2xs text-text-secondary hover:text-text-primary';
