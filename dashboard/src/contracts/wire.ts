// Thin re-export of the single canonical wire boundary.
//
// The wire contract now lives in one place, `./generated`, hand-matched to the
// Rust serde model in `src/dashboard/read_model.rs`. This file remains only as
// a stable import path for existing callers; every schema, type, and name
// re-exports unchanged, so `contracts/wire` and `contracts/generated` refer to
// exactly the same module. Prefer importing from `./generated` (or the package
// index) in new code.
export * from './generated';
