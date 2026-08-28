# Final concept-set cleanup

## Retained authority

- 35 reviewed PNG plates under the fourteen screen-specific `final/` folders.
- 35 same-stem product briefs and fourteen `final/README.md` manifests.
- The deterministic PR #743 HTML source beside its rendered plate.
- [DESIGN-SYSTEM.md](DESIGN-SYSTEM.md), [NAVIGATION.md](NAVIGATION.md), [INTERACTION-STATES.md](INTERACTION-STATES.md), and [IMPLEMENTATION.md](IMPLEMENTATION.md).
- The screen-level READMEs, which route implementation readers to the final manifests and record production authority boundaries.

## Removed concept-only cruft

After the replacement sets were accepted and manifest-linked, 76 rejected or superseded plate stems were removed from the branch tip. Each stem represented one PNG and one Markdown sidecar, for 152 files total. Git history through `e9a30ad1d` remains the recovery path.

- `01-brain` (14): `showcase-synapse-fired`, `v1-recency-field`, `v10-activity-becomes-synapse`, `v2-hook-synapses-admitted-ingress`, `v2-hook-synapses-meat-rejected`, `v2-hook-synapses`, `v3-hud-pass-dark`, `v3-hud-pass-light`, `v4-measured-registry`, `v5-luminous-idle-registry`, `v6-hover-focus`, `v7-real-activity-synapse`, `v8-activity-becomes-synapse`, `v9-activity-becomes-synapse`.
- `02-explorer` (5): `v1-three-lanes`, `v2-four-lanes`, `v3-hud-pass-dark`, `v3-hud-pass-light`, `v4-lane-lifecycle`.
- `03-loom` (8): `v1-weave`, `v2-hud-pass-dark`, `v2-hud-pass-light`, `v3-measured-weave`, `v4-luminous-measured-weave`, `v5-host-weave-overview`, `v6-host-weave-overview`, `v7-host-weave-overview`.
- `04-sessions` (4): `v1-inspector`, `v2-hud-pass-dark`, `v2-hud-pass-light`, `v3-provenance-inspector`.
- `05-agents` (4): `v1-host-tree`, `v2-hud-pass-dark`, `v2-hud-pass-light`, `v3-authority-tree`.
- `06-code` (4): `v1-cortex`, `v2-hud-pass-dark`, `v2-hud-pass-light`, `v3-lenses`.
- `07-knowledge` (5): `v1-single-view`, `v2-four-cameras`, `v3-hud-pass-dark`, `v3-hud-pass-light`, `v4-four-cameras`.
- `08-delivery` (4): `v1-recency-field`, `v2-hud-pass-dark`, `v2-hud-pass-light`, `v3-independent-authorities`.
- `09-automations` (4): `v1-cron-strip`, `v2-hud-pass-dark`, `v2-hud-pass-light`, `v3-scheduler-ledger`.
- `10-observatory` (6): `v0-radial-first`, `v1-radial`, `v2-overview-stack`, `v3-hud-pass-dark`, `v3-hud-pass-light`, `v4-overview-honest`.
- `11-costs` (4): `v1-provider-burn`, `v2-hud-pass-dark`, `v2-hud-pass-light`, `v3-provider-spend`.
- `12-settings` (5): `v1-layer-cake`, `v2-effective-values`, `v3-hud-pass-dark`, `v3-hud-pass-light`, `v4-effective-only`.
- `13-work` (5): `v1-nine-routes`, `v2-six-cameras`, `v3-hud-pass-dark`, `v3-hud-pass-light`, `v4-six-cameras`.
- `14-workflows` (4): `v1-lifecycle-tracks`, `v2-hud-pass-dark`, `v2-hud-pass-light`, `v3-definition-ledger`.

The internal Task 2 SDD report was also removed because it was process-only, was not referenced by the final briefs, and duplicated validation now enforced against the authoritative final manifests.

## Cleanup rule

No unrelated source, production dashboard code, plan authority, or historical material referenced by a final brief was removed. Future concept revisions must replace a final plate and its same-stem brief together, update the owning manifest, and record any subsequent cleanup here.
