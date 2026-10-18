use std::path::{Path, PathBuf};

use serde_json::json;

use super::{
    CodexContextState, CodexMeta, PRIOR_CONTEXT_CHUNK_BYTES, evict_prior_context_for_test,
};

const GENERATION: u64 = 7;

fn meta() -> CodexMeta {
    CodexMeta {
        cwd: PathBuf::from("/workspace"),
        session_id: "session.fixture".to_owned(),
        model: None,
        git: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        agent_nickname: None,
        agent_role: None,
        thread_source: None,
    }
}

fn line(record: serde_json::Value) -> String {
    record.to_string() + "\n"
}

fn message(index: usize) -> String {
    line(json!({
        "type": "event_msg",
        "payload": {"type": "user_message", "message": format!("message {index:04} {}", "x".repeat(64))}
    }))
}

/// A rollout whose session meta sets `/a`, followed by a long first turn,
/// a turn context setting `/b`, records that leave the context alone, and a
/// final turn context setting `/d`. Returns the offsets of the `/b` turn
/// context and the start and end of the `/d` one.
fn rollout(path: &Path) -> (u64, u64, u64) {
    let mut contents = line(json!({
        "type": "session_meta",
        "payload": {"id": "session.fixture", "cwd": "/a"}
    }));
    contents.extend((0..300).map(message));
    let set_b = contents.len() as u64;
    contents += &line(json!({
        "type": "turn_context",
        "payload": {"cwd": "/b", "model": "m-b"}
    }));
    contents += &line(json!({"type": "turn_context", "payload": {"turn_id": "turn.2"}}));
    contents += &line(json!({
        "type": "session_meta",
        "payload": {"id": "session.other", "cwd": "/c", "model": "m-c"}
    }));
    contents.extend((300..310).map(message));
    let set_d = contents.len() as u64;
    contents += &line(json!({
        "type": "turn_context",
        "payload": {"cwd": "/d", "model": "m-d"}
    }));
    let after_d = contents.len() as u64;
    contents += &message(310);
    std::fs::write(path, contents).unwrap();
    (set_b, set_d, after_d)
}

#[test]
fn prior_context_is_the_last_cwd_set_before_the_cursor() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("rollout.jsonl");
    let (set_b, set_d, after_d) = rollout(&path);
    let meta = meta();

    let (state, read) = CodexContextState::scan_prior(&path, GENERATION, set_d, &meta);
    assert_eq!(state.cwd, Path::new("/b"));
    assert_eq!(state.model.as_deref(), Some("m-b"));
    assert_eq!(
        read, PRIOR_CONTEXT_CHUNK_BYTES,
        "the walk stops at the current turn instead of reading the {set_d}-byte prefix"
    );

    evict_prior_context_for_test(&path);
    let (inside_record, _) = CodexContextState::scan_prior(&path, GENERATION, set_d + 10, &meta);
    assert_eq!(
        inside_record.cwd,
        Path::new("/b"),
        "the record the cursor sits inside is not prior context"
    );
    assert_eq!(inside_record.model.as_deref(), Some("m-b"));

    evict_prior_context_for_test(&path);
    let (first_turn, read) = CodexContextState::scan_prior(&path, GENERATION, set_b, &meta);
    assert_eq!(first_turn.cwd, Path::new("/a"));
    assert_eq!(first_turn.model, None);
    assert_eq!(
        read, set_b,
        "without a turn context the walk reaches the session meta"
    );

    let (replayed, read) = CodexContextState::scan_prior(&path, GENERATION, after_d, &meta);
    assert_eq!(replayed.cwd, Path::new("/d"));
    assert_eq!(replayed.model.as_deref(), Some("m-d"));
    assert_eq!(
        read,
        after_d - set_b,
        "a later cursor walks back no further than the cached one"
    );
}

#[test]
fn another_generation_does_not_resume_the_cached_context() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("rollout.jsonl");
    let (_, set_d, _) = rollout(&path);
    let meta = meta();
    let (cached, _) = CodexContextState::scan_prior(&path, GENERATION, set_d, &meta);
    assert_eq!(cached.cwd, Path::new("/b"));

    let contents = std::fs::read_to_string(&path).unwrap();
    let rewritten = contents.replacen(r#""cwd":"/b""#, r#""cwd":"/e""#, 1);
    assert_ne!(rewritten, contents);
    std::fs::write(&path, rewritten).unwrap();

    let (rewound, read) = CodexContextState::scan_prior(&path, GENERATION + 1, set_d, &meta);
    assert_eq!(
        rewound.cwd,
        Path::new("/e"),
        "a rewrite's generation reads the cwd from its own bytes, not the cached one"
    );
    assert_eq!(rewound.model.as_deref(), Some("m-b"));
    assert_eq!(read, PRIOR_CONTEXT_CHUNK_BYTES);
}

#[test]
fn turn_context_without_cwd_keeps_the_current_one() {
    let meta = meta();
    let mut state = CodexContextState::from_meta(&meta);
    assert!(state.observe_context_record(
        &json!({"type": "turn_context", "payload": {"cwd": "/b"}}),
        Path::new("/tmp/rollout.jsonl"),
        &meta,
    ));
    assert!(state.observe_context_record(
        &json!({"type": "turn_context", "payload": {"model": "m-new"}}),
        Path::new("/tmp/rollout.jsonl"),
        &meta,
    ));
    assert!(state.observe_context_record(
        &json!({"type": "turn_context", "payload": {}}),
        Path::new("/tmp/rollout.jsonl"),
        &meta,
    ));
    assert!(state.observe_context_record(
        &json!({"type": "session_meta", "payload": {"id": "session.other", "cwd": "/c", "model": "m-c"}}),
        Path::new("/tmp/rollout.jsonl"),
        &meta,
    ));
    assert!(!state.observe_context_record(
        &json!({"type": "event_msg", "payload": {"type": "user_message", "message": "hi"}}),
        Path::new("/tmp/rollout.jsonl"),
        &meta,
    ));
    assert_eq!(state.cwd, Path::new("/b"));
    assert_eq!(state.model.as_deref(), Some("m-new"));
}
