#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DoctorGraphSchemaState {
    ReleasedV0067,
    PreviousV2Candidate,
    Current,
    Unsupported,
}

pub(super) fn doctor_graph_schema_state(actual: i64) -> DoctorGraphSchemaState {
    match actual {
        18 => DoctorGraphSchemaState::ReleasedV0067,
        // V2 development builds stamped 24 through 26 before the final shape
        // settled; they are refused at open but diagnosed distinctly from
        // stores no supported binary ever produced.
        24..=26 => DoctorGraphSchemaState::PreviousV2Candidate,
        actual if actual == i64::from(crate::db::migrations::SCHEMA_VERSION) => {
            DoctorGraphSchemaState::Current
        }
        _ => DoctorGraphSchemaState::Unsupported,
    }
}
