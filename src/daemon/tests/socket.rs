use super::*;

#[test]
fn tool_json_payload_requires_exactly_one_json_block() {
    let valid = serde_json::json!({
        "content": [
            {"text": "status"},
            {"text": "{\"ok\":true}"}
        ]
    });
    assert_eq!(
        super::super::tool_json_payload(&valid, "test").unwrap(),
        serde_json::json!({"ok": true})
    );

    for (content, expected) in [
        (
            serde_json::json!([{"text": "{\"first\":1}"}, {"text": "[2]"}]),
            "returned multiple JSON payloads",
        ),
        (
            serde_json::json!([{"text": "status"}, {"type": "image"}]),
            "returned no JSON payload",
        ),
    ] {
        let error =
            super::super::tool_json_payload(&serde_json::json!({"content": content}), "test")
                .unwrap_err();
        assert!(error.to_string().contains(expected));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn socket_client_rejects_tool_calls_without_project() {
    let home = TempDir::new().expect("home");
    let home = home.path().canonicalize().expect("canonical home");
    let client_identity = test_client_identity_for(home.join("client"));

    let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
    let server_task = tokio::spawn(super::super::serve_socket_client(
        server,
        super::super::DaemonEngine::default(),
    ));

    let (reader, mut writer) = client.into_split();
    let handshake = DaemonHandshake {
        client_identity,
        ..test_handshake_defaults()
    };
    writer
        .write_all(handshake.to_line().expect("handshake").as_bytes())
        .await
        .expect("write handshake");
    writer.write_all(b"\n").await.expect("newline");
    writer
        .write_all(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_lcm_status",
                    "arguments": {
                        "provider": "cursor",
                        "format": "json"
                    }
                }
            }))
            .expect("tools/call json")
            .as_bytes(),
        )
        .await
        .expect("write tools/call");
    writer.write_all(b"\n").await.expect("newline");
    writer.shutdown().await.expect("shutdown writer");

    let mut lines = tokio::io::BufReader::new(reader).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("projectless rejection should not time out")
        .expect("read response")
        .expect("projectless response");
    let response: Value = serde_json::from_str(&line).expect("response json");
    assert_eq!(response["id"], json!(7));
    assert_eq!(
        response["error"]["message"], "tracedecay_lcm_status requires an initialized code project",
        "projectless handshake should return the stable current contract"
    );

    server_task
        .await
        .expect("server task should complete")
        .expect("projectless client shutdown should be clean");
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_linked_worktree_route_repairs_primary_identity_and_keeps_alias() {
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical temp dir");
    let primary = root.join("primary");
    let linked = root.join("linked");
    let profile_root = root.join("profile");
    std::fs::create_dir_all(&primary).expect("primary dir");
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "TraceDecay Test")
            .env("GIT_AUTHOR_EMAIL", "test@tracedecay.local")
            .env("GIT_COMMITTER_NAME", "TraceDecay Test")
            .env("GIT_COMMITTER_EMAIL", "test@tracedecay.local")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&primary, &["init", "-b", "main", "--quiet"]);
    std::fs::write(primary.join("README.md"), "linked worktree route\n").expect("fixture");
    git(&primary, &["add", "."]);
    git(&primary, &["commit", "-m", "fixture", "--quiet"]);
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "feature/linked-route",
            linked.to_str().expect("utf-8 linked path"),
            "HEAD",
        ],
    );

    let client_identity = test_client_identity_for(profile_root.clone());
    let options = crate::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(client_identity.global_db_path.clone()),
    };
    let primary_cg = crate::tracedecay::TraceDecay::init_with_options(&primary, options.clone())
        .await
        .expect("primary init");
    primary_cg.index_all().await.expect("primary index");
    primary_cg
        .db()
        .checkpoint()
        .await
        .expect("primary checkpoint");
    let project_id = primary_cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("profile project id");
    drop(primary_cg);
    let mut config = crate::config::load_config(&linked).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&linked, &config)
        .expect("disable unrelated startup transcript ingestion");

    let registry = crate::global_db::GlobalDb::open_at(&client_identity.global_db_path)
        .await
        .expect("registry");
    registry
        .upsert_code_project(
            &project_id,
            &linked,
            crate::worktree::git_common_dir(&linked).as_deref(),
            None,
            Some("main"),
        )
        .await
        .expect("seed stale linked canonical root");

    let handshake = DaemonHandshake {
        project_path: Some(linked.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "linked-worktree-route-test")
            .expect("daemon database scope");
    let engine = super::super::DaemonEngine::default();
    engine
        .project_server(&handshake)
        .await
        .expect("daemon linked-worktree route");

    let context = registry
        .project_registry_context_by_id(&project_id)
        .await
        .expect("registry context");
    assert_eq!(
        context.project.canonical_root,
        crate::global_db::GlobalDb::canonical_project_key(&primary)
    );
    assert!(context.aliases.iter().any(|alias| {
        alias.alias_path == crate::global_db::GlobalDb::canonical_project_key(&linked)
    }));
}

#[test]
fn unsupported_daemon_transport_never_falls_back_to_local_sqlite() {
    assert!(super::super::proxy_required_by_platform(false, false));
    assert!(super::super::proxy_required_by_platform(false, true));
    assert!(!super::super::proxy_required_by_platform(true, false));
}
