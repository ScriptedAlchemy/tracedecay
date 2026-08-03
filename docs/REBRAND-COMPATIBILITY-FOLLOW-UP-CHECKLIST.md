# TraceDecay rebrand compatibility follow-up checklist

> **Retired backlog — not implementation authority.** The former ticket-sized
> checklist mixed a pre-V2 storage conversion story with unrelated source and
> installer cleanup. Current scope comes from the V2 plans and
> [`REBRAND-COMPATIBILITY-POLICY.md`](REBRAND-COMPATIBILITY-POLICY.md).

## Scope guardrail

This policy applies only to independently released public interfaces,
installed-agent artifacts, and user-owned pre-rebrand data with evidence of an
external contract. Fresh V2 profiles use the final product shape. Do not add an
internal store reader, converter, backfill, dual write, profile census, or
staged cutover from this checklist.

## Retained review work

1. **Released public inputs.** When an externally released CLI, API, SDK, or
   configuration spelling is retained, document the evidence, route it through
   the canonical operation, and test old-only, new-only, and conflict behavior
   without exposing secrets.
2. **Owned generated integration artifacts.** Install, refresh, and uninstall
   may reconcile only artifacts whose ownership is proven. Preserve unknown
   user-authored files and test repeatability of the real integration journey.
3. **Truthful documentation.** Canonical names lead active docs. A legacy
   fallback is documented only when current implementation proves it; otherwise
   remove the claim rather than creating a compatibility promise.
4. **External user-data safety.** Never implicitly rename, delete, or move a
   pre-rebrand data root. A separately released user-data move requires explicit
   user intent, backups, validation, and reversible cleanup; it is not a V2
   internal-store conversion task.

## Explicit non-goals

- One-time migration of Hermes-local stores, project pins, or profile state.
- Branch/worktree-scoped fact storage, archive merges, or data movement based
  only on development history.
- Unverified plugin-path fallbacks, source-string inventories, exact test-count
  gates, and archived task matrices.

## Review prompt

Before changing a rebrand-related surface, classify it with
[`REBRAND-COMPATIBILITY-POLICY.md`](REBRAND-COMPATIBILITY-POLICY.md): is it an
independently shipped public contract, an owned generated artifact, external
user-owned data, or merely source-only V2 implementation detail? Preserve the
first three only within their documented boundary; change the last in place.
