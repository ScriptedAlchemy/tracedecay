pub(super) fn dashboard_http_request_deadline_micros(path: &str) -> i64 {
    if matches!(
        path,
        "/api/automation/run/memory-curator"
            | "/api/automation/run/session-reflection"
            | "/api/automation/run/skill-writing"
    ) || is_project_scoped_automation_run_path(path)
        || is_user_job_run_path(path)
    {
        super::DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS
    } else {
        super::DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS
    }
}

fn is_user_job_run_path(path: &str) -> bool {
    let Some(job_and_action) = path.strip_prefix("/api/automation/jobs/") else {
        return false;
    };
    let Some((job_id, action)) = job_and_action.split_once('/') else {
        return false;
    };
    !job_id.is_empty() && action == "run"
}

fn is_project_scoped_automation_run_path(path: &str) -> bool {
    let Some(project_and_tail) = path.strip_prefix("/api/projects/") else {
        return false;
    };
    let Some((project_id, tail)) = project_and_tail.split_once('/') else {
        return false;
    };
    !project_id.is_empty()
        && matches!(
            tail,
            "automation/run/memory-curator"
                | "automation/run/session-reflection"
                | "automation/run/skill-writing"
        )
}
