use tracedecay_runtime_core::logging::log_daemon_event;

pub fn log_code_generation_retention_degraded(
    observations: &crate::telemetry::StoreTelemetrySamplingRegistry,
    failure: &str,
) {
    observations.mark_loud_retention_log();
    log_daemon_event(
        "retention_degraded",
        &[
            ("pass", "code_generations".to_owned()),
            ("failure", failure.to_owned()),
        ],
    );
}
