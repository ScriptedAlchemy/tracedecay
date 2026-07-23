// Public entry point for the dashboard wire contracts.
//
// There is exactly one wire-boundary module — `./generated` — hand-matched to
// the Rust serde model in `src/dashboard/read_model.rs`. `./wire` is a thin
// re-export of the same module kept for import-path stability.
export * from './generated';
