/**
 * Observatory's storage-finding names, re-exported from the generated barrel.
 *
 * This module used to declare the storage-findings wire shape by hand —
 * `DoctorFindingsPayloadSchema.extend({ kind_statuses: … .length(5) })` — which
 * was wrong twice over. It extended the wrong payload (the route serves
 * `StorageFindingsPayloadV1`, not `DoctorFindingsPayloadV1`), and it pinned the
 * Plan 38 producer count at exactly five in TypeScript, so adding a sixth
 * storage finding kind in Rust would have made every real response fail to
 * parse and render as `unsupported_schema` — a live storage report replaced by
 * a schema error, which is precisely the failure the generated boundary exists
 * to make impossible.
 *
 * `StorageFindingsPayloadV1` already carries `kind_statuses`, and
 * `StorageFindingKindStatusV1` is field-for-field what was written here. So
 * there is nothing left to declare: the names below are aliases kept so callers
 * keep a stable import path, and the shapes are the generated ones.
 */
export {
  StorageFindingsPayloadSchema as ObservatoryStorageFindingsPayloadSchema,
  StorageFindingSourceStateSchema,
  StorageFindingKindStatusSchema as StorageFindingSourceStatusSchema,
} from '../../contracts/wire.ts';

export type {
  StorageFindingsPayload as ObservatoryStorageFindingsPayload,
  StorageFindingSourceState,
  StorageFindingKindStatus as StorageFindingSourceStatus,
} from '../../contracts/wire.ts';
