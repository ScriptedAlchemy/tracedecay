# Workflows concept plates

## Purpose

Workflows is the immutable definition, validation, lifecycle, and run-history
surface: users inspect exactly what a version pins and does, then perform only
authorized daemon-validated lifecycle changes with compare-and-swap receipts.

Route: `/workflows`.

## Authoritative final set

[`final/README.md`](final/README.md) is the implementation authority for the
reviewed Workflows state set. Its definition-lifecycle ledger and same-stem
brief define the current product, evidence, permission, scale, and
accessibility contract.

Historical plates below are retained for design provenance only. None is a
current implementation reference.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and evidence language;
  [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage;
  [IMPLEMENTATION.md](../IMPLEMENTATION.md) owns the browser/scene boundary.
- `dashboard/src/workspaces/workflows/WorkflowsPage.tsx` and
  `workflowQueries.ts` use canonical `/application/workflow` routes for
  definitions, lifecycle, and run projection; generated contracts decode each
  rendered value.
- The final plate remains concept/synthetic. These source paths identify
  production authorities, not proof that the pictured data or controls are live.

## Canonical semantic-state matrix

| Semantic state or interaction | Authoritative brief | Entry condition |
|---|---|---|
| Definition registry and selected version | [final/01-definition-lifecycle-ledger.md](final/01-definition-lifecycle-ledger.md) | Definition registry serves a page and stable selection. |
| Immutable pins and decoded steps | [final/01-definition-lifecycle-ledger.md](final/01-definition-lifecycle-ledger.md) | Selected version and referenced policy/config/catalog are readable. |
| Validation and permission result | [final/01-definition-lifecycle-ledger.md](final/01-definition-lifecycle-ledger.md) | Daemon validates exact version and caller authority. |
| Activate, retire, or reject | [final/01-definition-lifecycle-ledger.md](final/01-definition-lifecycle-ledger.md) | Prepared command, permission, validation, and expected revision are served. |
| CAS conflict, refusal, denial, or unavailable | [final/01-definition-lifecycle-ledger.md](final/01-definition-lifecycle-ledger.md) | Lifecycle authority fails closed with a typed result. |
| Lifecycle history | [final/01-definition-lifecycle-ledger.md](final/01-definition-lifecycle-ledger.md) | Registry serves immutable transition receipts. |
| Exact run lookup and run history | [final/01-definition-lifecycle-ledger.md](final/01-definition-lifecycle-ledger.md) | Run projection serves exact ID or an honest empty/denied/unavailable state. |

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
