#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DoctorGraphSchemaState {
    Current,
    Unsupported,
}

pub(super) fn doctor_graph_schema_state(actual: i64) -> DoctorGraphSchemaState {
    match actual {
        actual if actual == i64::from(tracedecay_runtime_core::db::migrations::SCHEMA_VERSION) => {
            DoctorGraphSchemaState::Current
        }
        _ => DoctorGraphSchemaState::Unsupported,
    }
}
