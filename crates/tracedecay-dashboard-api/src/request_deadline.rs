pub(super) fn dashboard_http_request_deadline_micros(path: &str) -> i64 {
    let automation_run = path == "/api/application/retained/fact_store_curate"
        || one_segment_then(
            path,
            "/api/projects/",
            "application/retained/fact_store_curate",
        )
        || one_segment_then(path, "/api/automation/jobs/", "run");
    if automation_run {
        super::DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS
    } else {
        super::DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS
    }
}

/// `{prefix}{nonempty}/{tail}` with `tail` compared in full, including slashes.
fn one_segment_then(path: &str, prefix: &str, tail: &str) -> bool {
    let Some(rest) = path.strip_prefix(prefix) else {
        return false;
    };
    let Some((head, rest_tail)) = rest.split_once('/') else {
        return false;
    };
    !head.is_empty() && rest_tail == tail
}
