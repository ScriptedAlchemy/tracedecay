//! PR9 code-intelligence indexing module spine (Plan 25: Code Intelligence
//! Indexing).
//!
//! This module tree holds the initial implementation home of the Plan 25
//! contracts defined in `tracedecay_domain::code_intelligence`. PR9 starts as
//! focused root modules; `tracedecay-code-index` extraction happens only if
//! the Plan 19 measured gate approves it, and moves this tree without
//! changing the domain values or ports (see
//! `docs/plans/tracedecay-v2/pr9/00-contract-spine.md`).
//!
//! This spine is contracts only: intake validation, language registry,
//! extraction, chunks, and capability emission port traits. No parser
//! acquisition, filesystem access, storage, or scheduling lives here;
//! capture owns intake snapshots, the store owns persistence, and the
//! projector composition owns publication (Plan 25 boundaries).

pub mod capabilities;
pub mod chunks;
pub mod extract;
pub mod intake;
pub mod languages;

pub use self::intake::CodeIndexIntake;
