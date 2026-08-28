---
design_status: current
evidence_class: concept_synthetic
---

# Settings — effective configuration review

- **Asset:** `01-effective-configuration-review.png`
- **Route:** `/settings`
- **Lifecycle:** authoritative final concept

## User job

Find the effective value TraceDecay is actually using, understand which configuration layer supplied or overrode it, make an authorized change safely, know whether it applies immediately or requires restart/reindex/reconnect, and recover from validation or concurrent-edit conflicts.

## Product behavior

- Search and sections operate over effective configuration. Every row shows key, effective value, source layer, origin, write capability, and apply requirement when those fields are served.
- Selecting a row opens a review state with current value, proposed value, target layer, validation result, current compare-and-swap revision, and impact such as immediate, daemon restart, reindex, reconnect, or next-run.
- Layer order is explicit: built-in/default, organization policy, system, user/profile, project, worktree/session, and temporary runtime override where supported. A hidden or unserved provenance field is labeled `unserved`, never inferred from the effective value.
- Apply is enabled only when the real settings write endpoint, authorization, target layer, schema validation, and current revision are all available. The result is typed as applied, rejected, conflicted, denied, invalid, unavailable, or restart-required. A concept control is not evidence that save exists.
- Multi-root and Remote Brain panels expose operational/read states independently of configuration values and cross-link to the owning status view.

## Production authorities

- `SettingsPage.tsx` decodes `/api/settings` with `SettingsPayloadV1Schema` and `DashboardEnvelopeV1Schema`.
- `SettingsEditorController.tsx` owns production write gates and compare-and-swap behavior; `MultiRootPanel.tsx` and `RemoteBrainPanel.tsx` own their independent operational states.
- The effective-configuration projection owns resolved values and served provenance. Schema validation owns type/range validity. The write authority owns authorization, target-layer persistence, CAS, and typed outcome. Runtime components own restart/reindex/reconnect completion.
- Shared shell, project scope, and state language come from [navigation](../../NAVIGATION.md), [design system](../../DESIGN-SYSTEM.md), and [interaction states](../../INTERACTION-STATES.md).

## Canonical evidence ladder

From strongest to weakest: persisted layer value and exact revision; server-resolved effective value with exact provenance; effective value with provenance explicitly unserved; validated local proposal not yet applied; stale snapshot; unavailable, denied, invalid, or conflicted state. A proposal is never shown as the effective value before an authoritative apply response and read-back.

The UI must distinguish effective, inherited, overridden, edited-not-applied, applied-restart-required, conflict, denied, unavailable, and provenance-unserved. Validation success does not mean persistence success or runtime adoption.

## Interaction and scale contract

- Keyboard: search, section navigation, rows, review fields, and actions are fully operable; focus returns to the edited row after cancel/apply/conflict resolution. Destructive or high-impact changes require explicit review.
- Reduced motion: provenance/layer transitions and apply progress use static state changes and text; success is never conveyed by animation alone.
- 200% zoom/reflow: section navigation becomes a labeled disclosure/menu, the row table becomes key/value cards or a scrollable labeled region, and the review panel takes full width without clipping values or validation errors.
- Dense data: virtualize large configuration registries, support key/value/source filtering, preserve stable row identity, and keep the selected review state pinned without rendering every key at once.
- Exact fallback: provide an accessible effective-configuration table containing key, value, source layer, origin, revision/freshness, write capability, and apply requirement; validation and apply receipts remain readable text.

## Truth boundary

This reviewed plate is `CONCEPT / SYNTHETIC DATA`. Its keys, values, revisions, timestamps, roots, Remote Brain states, and edited patch are illustrative. The pictured Apply control must call the real authorized compare-and-swap production path; until that path is available it is disabled and labeled unavailable. No fake save, optimistic success, fabricated provenance, or silent restart claim is permitted.
