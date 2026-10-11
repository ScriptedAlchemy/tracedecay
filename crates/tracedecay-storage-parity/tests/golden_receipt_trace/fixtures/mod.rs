//! Deterministic payload fixtures. Every id, digest and timestamp is fixed so
//! two runs on any engine submit byte-identical requests.

pub mod effects;
pub mod observation;
pub mod project;
pub mod stores;

pub const BRAIN: &str = "brain.parity";
pub const PROFILE: &str = "profile.parity";
pub const PROJECT: &str = "project.parity";

pub fn project_id() -> tracedecay_domain::ProjectId {
    tracedecay_domain::ProjectId::new(PROJECT).expect("project id")
}
