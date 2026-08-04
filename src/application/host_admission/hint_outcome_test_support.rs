use super::*;

impl HostAdmissionTestRuntimeV1 {
    pub(crate) async fn correlate_hint_outcomes_for_test(
        &self,
        scope: HostAdmissionScope,
        project_id: &str,
        now: i64,
    ) -> Result<crate::hooks::hint_outcomes::HintOutcomeStats> {
        crate::application::hint_outcomes::correlate_registered_hint_outcomes(
            self.profile_database.as_ref(),
            self.session_database_for_test(scope)?,
            project_id,
            now,
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "correlate hook hint outcomes".to_owned(),
            message: error.to_string(),
        })
    }
}
