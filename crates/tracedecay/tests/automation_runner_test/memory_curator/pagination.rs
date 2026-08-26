use std::sync::Mutex;

use super::*;

struct ReviewFactRecordingBackend {
    seen_fact_ids: Mutex<Vec<String>>,
}

impl ReviewFactRecordingBackend {
    fn new() -> Self {
        Self {
            seen_fact_ids: Mutex::new(Vec::new()),
        }
    }

    fn seen_fact_ids(&self) -> Vec<String> {
        self.seen_fact_ids.lock().unwrap().clone()
    }
}

impl AgentTaskBackend for ReviewFactRecordingBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> std::result::Result<AgentTaskResponse, tracedecay_automation::backend::AgentTaskError>
    {
        let fact_id = request.context["llm_review"]["facts"][0]["fact_id"]
            .as_str()
            .expect("review page exposes one canonical fact")
            .to_owned();
        self.seen_fact_ids.lock().unwrap().push(fact_id);
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: "{\"ops\":[]}".to_owned(),
            output_json: Some(json!({"ops": []})),
            model: Some("pagination-fixture".to_owned()),
            provider: Some("fixture".to_owned()),
            input_tokens: None,
            output_tokens: None,
        })
    }
}

#[tokio::test]
async fn memory_curator_resumes_from_the_durable_next_page_cursor() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let facts = seed_duplicate_facts(&cg).await;
    let backend = ReviewFactRecordingBackend::new();
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        ..AutomationConfig::default()
    };
    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let options = || MemoryCuratorAutomationOptions {
        trigger: AutomationTrigger::ManualCli,
        fact_review_limit: 1,
        min_confidence: 0.5,
        run_id: None,
    };

    for _ in 0..2 {
        tracedecay_agent_hosts::automation::runner::run_memory_curator_with_backend(
            &cg,
            &config,
            &test_configuration_revision(),
            &backend,
            options(),
            &run_control,
        )
        .await
        .unwrap();
    }

    let seen = backend.seen_fact_ids();
    assert_eq!(seen.len(), 2);
    assert_ne!(
        seen[0], seen[1],
        "the second run must advance past page one"
    );
    let mut seen_sorted = seen;
    seen_sorted.sort();
    let mut expected = vec![facts.winner_id, facts.loser_id];
    expected.sort();
    assert_eq!(seen_sorted, expected);
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(
        records[0].validation_report.as_ref().unwrap()["pagination"]["resume_after_fact_id"],
        Value::Null,
        "the last page durably records wrap-around"
    );
}
