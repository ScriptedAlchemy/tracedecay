use tempfile::TempDir;
use tracedecay_sessions::runtime::SessionProvider;

use crate::restart_atomicity::{ingest_global_sources_for_provider, open_project_session_db};
use crate::support::{assert_path_text_eq, setup};

fn wire_message(agent: &str, role: &str, text: &str) -> String {
    serde_json::json!({
        "type": "context.append_message",
        "agentId": agent,
        "message": {
            "role": role,
            "content": [{"type": "text", "text": text}],
            "toolCalls": []
        },
        "time": 1_789_228_081_157_i64
    })
    .to_string()
        + "\n"
}

/// A Kimi session feeds one canonical session from every agent wire in its
/// directory; the main agent's `wire.jsonl` is the session's source
/// transcript.
#[tokio::test]
async fn kimi_main_agent_wire_is_the_session_transcript_path() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let session_dir = home.join(".kimi-code/sessions/wd_project/session-kimi");
    let main_wire = session_dir.join("agents/main/wire.jsonl");
    let sub_wire = session_dir.join("agents/sub-1/wire.jsonl");
    std::fs::create_dir_all(main_wire.parent().unwrap()).unwrap();
    std::fs::create_dir_all(sub_wire.parent().unwrap()).unwrap();
    std::fs::write(
        session_dir.join("state.json"),
        serde_json::json!({
            "id": "session-kimi",
            "cwd": project,
            "agents": {
                "main": {"type": "main"},
                "sub-1": {"type": "sub"}
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        &main_wire,
        format!(
            "{}{}",
            wire_message(
                "main",
                "user",
                "Investigate the billing pipeline regression"
            ),
            wire_message(
                "main",
                "assistant",
                "The billing pipeline regression is fixed."
            )
        ),
    )
    .unwrap();
    std::fs::write(
        &sub_wire,
        wire_message(
            "sub-1",
            "assistant",
            "sub-agent verified the billing pipeline fix",
        ),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Kimi)).await;

    let session = db
        .get_session("kimi", "session-kimi")
        .await
        .expect("kimi session should be stored");
    assert_path_text_eq(
        session
            .transcript_path
            .as_deref()
            .expect("session transcript path"),
        &main_wire,
    );
    assert_eq!(
        db.search_session_messages("kimi", None, "billing pipeline", 10)
            .await
            .len(),
        3
    );
}
