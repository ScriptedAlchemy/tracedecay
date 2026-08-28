# Settings concept plates

## Purpose

Effective configuration boundaries without fabricated provenance or remote query.

Route: `/settings`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/settings/SettingsPage.tsx` decodes `/api/settings` with `SettingsPayloadV1Schema` and `DashboardEnvelopeV1Schema`; `SettingsEditorController.tsx`, `MultiRootPanel.tsx`, and `RemoteBrainPanel.tsx` own write gates and remote state.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Effective values and layer provenance | [final/01-effective-configuration-review.md](final/01-effective-configuration-review.md) | Open `/settings`; the effective projection returns each key and any served source layer/origin. |
| Writable, locked, denied, or unavailable rows | [final/01-effective-configuration-review.md](final/01-effective-configuration-review.md) | The write authority reports capability for the exact key and target layer. |
| Proposed edit and validation | [final/01-effective-configuration-review.md](final/01-effective-configuration-review.md) | Edit a writable value; schema validation returns a typed result before apply. |
| Compare-and-swap review and conflict | [final/01-effective-configuration-review.md](final/01-effective-configuration-review.md) | Review against the current config revision; reject a changed revision as conflict. |
| Apply requirement | [final/01-effective-configuration-review.md](final/01-effective-configuration-review.md) | A successful persisted change reports immediate, restart, reindex, reconnect, or next-run adoption. |
| Multi-root operational state | [final/01-effective-configuration-review.md](final/01-effective-configuration-review.md) | Multi-root authority reports configured, unconfigured, stale, denied, or unavailable roots. |
| Remote Brain operational state | [final/01-effective-configuration-review.md](final/01-effective-configuration-review.md) | Remote authority reports connected, idle, unavailable, unconfigured, denied, or stale. |
| Search/filter and exact fallback | [final/01-effective-configuration-review.md](final/01-effective-configuration-review.md) | Search by key/value/source or switch to the accessible configuration table. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
