use super::*;

#[tokio::test]
async fn cancelled_ci_read_makes_no_network_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let client = GitHubCiReadOnlyClientV1 {
        agent: ureq::Agent::config_builder()
            .https_only(false)
            .http_status_as_error(false)
            .build()
            .into(),
        target: GitHubCiRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "tracedecay".to_owned(),
        },
        credential: GitHubReadOnlyCredentialV1::anonymous(),
        config: GitHubHttpReadConfigV1 {
            rest_base_uri: format!("http://{address}"),
            graphql_uri: format!("http://{address}/graphql"),
            ..GitHubHttpReadConfigV1::default()
        },
    };
    let request_scope = scope("cancelled-ci");
    let cancelled = context(&request_scope).with_cancellation(
        CancellationContext::cancelled("cancel.github.ci", UtcMicros(1)).unwrap(),
    );

    assert_eq!(
        client
            .read_workflow_runs_for_head(&cancelled, request_scope.head_commit_id.as_str(), 1,)
            .await,
        GitHubCiTransportOutcomeV1::Unavailable
    );
    tokio::task::yield_now().await;
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

pub(super) fn read_http_request_with_headers(
    stream: &mut TcpStream,
) -> (String, serde_json::Value) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "fixture client closed before request headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "fixture client closed before request body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = if content_length == 0 {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
    };
    (
        String::from_utf8(bytes[..header_end].to_vec()).unwrap(),
        body,
    )
}

pub(super) fn read_http_request(stream: &mut TcpStream) -> serde_json::Value {
    read_http_request_with_headers(stream).1
}

pub(super) fn write_http_json(stream: &mut TcpStream, value: &serde_json::Value) {
    let body = serde_json::to_vec(value).unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-RateLimit-Limit: 5000\r\nX-RateLimit-Remaining: 4999\r\nX-RateLimit-Reset: 2000000000\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
}

#[test]
fn expired_context_after_first_graphql_page_makes_no_nested_request() {
    let mut first_page: serde_json::Value = serde_json::from_str(THREAD_CAPTURE).unwrap();
    first_page = first_page["response"].take();
    let thread = &mut first_page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0];
    thread["comments"]["pageInfo"] = json!({
        "hasNextPage": true,
        "endCursor": "cursor.comments.expired"
    });

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        std::thread::sleep(Duration::from_millis(350));
        write_http_json(&mut stream, &first_page);
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        match listener.accept() {
            Ok((mut unexpected, _)) => {
                let _ = read_http_request(&mut unexpected);
                write_http_json(&mut unexpected, &json!({}));
                2
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => 1,
            Err(error) => panic!("nested request probe failed: {error}"),
        }
    });
    let client = GitHubReadOnlyClientV1 {
        agent: ureq::Agent::config_builder()
            .https_only(false)
            .http_status_as_error(false)
            .build()
            .into(),
        target: GitHubRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "tracedecay".to_owned(),
            pull_request_number: 421,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        },
        credential: GitHubReadOnlyCredentialV1::anonymous(),
        config: GitHubHttpReadConfigV1 {
            rest_base_uri: format!("http://{address}"),
            graphql_uri: format!("http://{address}/graphql"),
            ..GitHubHttpReadConfigV1::default()
        },
    };
    let owner_scope = scope("expired-page");
    let deadline = Deadline::new(UtcMicros(now_micros().0.saturating_add(250_000))).unwrap();
    let expired_during_read = context(&owner_scope).with_deadline(deadline);
    let outcome = client.execute_graphql(
        &expired_during_read,
        &GitHubGraphQlReadRequestV1 {
            scope: owner_scope,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
            resume: GitHubReadResumeV1::empty(),
        },
    );

    assert!(matches!(outcome, GitHubReadNetworkOutcomeV1::Denied));
    assert_eq!(server.join().unwrap(), 1);
}

#[test]
fn unregistered_credential_after_first_graphql_page_makes_no_nested_request() {
    let mut first_page: serde_json::Value = serde_json::from_str(THREAD_CAPTURE).unwrap();
    first_page = first_page["response"].take();
    let thread = &mut first_page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0];
    thread["comments"]["pageInfo"] = json!({
        "hasNextPage": true,
        "endCursor": "cursor.comments.revoked"
    });

    let (authority, resolution) = registered_fixture_credential(
        "revoked-after-page",
        FixtureCredentialAuthorityModeV1::Verified,
    );
    let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) = resolution else {
        panic!("verified authority must resolve");
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        assert!(unregister_github_read_only_credential_authority_v1(
            "ScriptedAlchemy",
            "revoked-after-page",
            &authority,
        ));
        write_http_json(&mut stream, &first_page);
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        match listener.accept() {
            Ok((mut unexpected, _)) => {
                let _ = read_http_request(&mut unexpected);
                write_http_json(&mut unexpected, &json!({}));
                2
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => 1,
            Err(error) => panic!("nested request probe failed: {error}"),
        }
    });
    let client = GitHubReadOnlyClientV1 {
        agent: ureq::Agent::config_builder()
            .https_only(false)
            .http_status_as_error(false)
            .build()
            .into(),
        target: GitHubRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "revoked-after-page".to_owned(),
            pull_request_number: 421,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        },
        credential,
        config: GitHubHttpReadConfigV1 {
            rest_base_uri: format!("http://{address}"),
            graphql_uri: format!("http://{address}/graphql"),
            ..GitHubHttpReadConfigV1::default()
        },
    };
    let owner_scope = scope("revoked-page");
    let outcome = client.execute_graphql(
        &context(&owner_scope),
        &GitHubGraphQlReadRequestV1 {
            scope: owner_scope,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
            resume: GitHubReadResumeV1::empty(),
        },
    );

    assert!(matches!(outcome, GitHubReadNetworkOutcomeV1::Denied));
    assert_eq!(server.join().unwrap(), 1);
}

#[tokio::test]
async fn github_nested_pagination_and_cas_are_owner_bound() {
    let mut first_page: serde_json::Value = serde_json::from_str(THREAD_CAPTURE).unwrap();
    first_page = first_page["response"].take();
    let thread = &mut first_page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0];
    thread["comments"]["pageInfo"] = json!({
        "hasNextPage": true,
        "endCursor": "cursor.comments.1"
    });
    let thread_id = thread["id"].as_str().unwrap().to_owned();
    let mut next_comment = thread["comments"]["nodes"][0].clone();
    next_comment["databaseId"] = json!(3_556_767_424_u64);
    next_comment["url"] =
        json!("https://github.com/ScriptedAlchemy/tracedecay/pull/421#discussion_r3556767424");
    serde_json::from_value::<GraphQlResponseV1>(first_page.clone())
        .expect("synthetic first page must satisfy the production response contract");
    let second_page = json!({
        "data": {
            "node": {
                "id": thread_id.clone(),
                "comments": {
                    "nodes": [next_comment],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }
    });

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let server = std::thread::spawn(move || {
        for response in [first_page, second_page] {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "production client did not request the expected GraphQL page"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("GraphQL fixture accept failed: {error}"),
                }
            };
            captured
                .lock()
                .unwrap()
                .push(read_http_request(&mut stream));
            write_http_json(&mut stream, &response);
        }
    });
    let config = GitHubHttpReadConfigV1 {
        rest_base_uri: format!("http://{address}"),
        graphql_uri: format!("http://{address}/graphql"),
        ..GitHubHttpReadConfigV1::default()
    };
    let client = GitHubReadOnlyClientV1 {
        agent: ureq::Agent::config_builder()
            .https_only(false)
            .http_status_as_error(false)
            .build()
            .into(),
        target: GitHubRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "tracedecay".to_owned(),
            pull_request_number: 421,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        },
        credential: GitHubReadOnlyCredentialV1::anonymous(),
        config,
    };
    let owner_scope = scope("owner");
    let read_request = request(owner_scope.clone());
    let read_context = context(&owner_scope);
    let outcome = client.execute_graphql(
        &read_context,
        &GitHubGraphQlReadRequestV1 {
            scope: owner_scope.clone(),
            pull_request_id: read_request.pull_request_id.clone(),
            resume: GitHubReadResumeV1::empty(),
        },
    );
    server.join().unwrap();
    let GitHubReadNetworkOutcomeV1::Response(response) = outcome else {
        panic!("production GraphQL client must complete nested pagination");
    };
    assert_eq!(
        response.metadata.rate_limit,
        Some(GitHubReviewRateLimitCheckpointV1 {
            limit: 5_000,
            remaining: 4_999,
            reset_at: UtcMicros(2_000_000_000_000_000),
        })
    );
    let envelope: GraphQlResponseV1 = serde_json::from_slice(&response.body).unwrap();
    let comments = &envelope
        .data
        .unwrap()
        .repository
        .unwrap()
        .pull_request
        .unwrap()
        .review_threads
        .nodes[0]
        .comments;
    assert_eq!(comments.nodes.len(), 2);
    assert!(!comments.page_info.has_next_page);
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["variables"]["loadThreads"], true);
        assert_eq!(requests[1]["variables"]["loadComments"], true);
        assert_eq!(requests[1]["variables"]["commentThreadId"], thread_id);
        assert_eq!(
            requests[1]["variables"]["commentAfter"],
            "cursor.comments.1"
        );
    }

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("github-owner-bound.db");
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(&path, "github owner-bound CAS").unwrap();
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .unwrap();
    let store =
        ProjectGitHubReviewStoreV1::new(database, owner_scope.clone()).expect("owner store");
    let context = context(&owner_scope);
    let state = GitHubReviewRefreshStateV1::transition(
        &read_request,
        None,
        complete_response(&read_request),
    )
    .unwrap();
    assert_eq!(
        store
            .compare_and_record(&context, &read_request, None, &state)
            .await,
        GitHubReviewRefreshStoreCommitOutcomeV1::Recorded
    );
    assert_eq!(
        store
            .compare_and_record(&context, &read_request, None, &state)
            .await,
        GitHubReviewRefreshStoreCommitOutcomeV1::Duplicate
    );

    let foreign_request = request(scope("foreign"));
    assert_eq!(
        store
            .compare_and_record(&context, &foreign_request, None, &state)
            .await,
        GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable
    );
    let mut latest = complete_response(&read_request);
    latest.ingress.fetched_at = UtcMicros(11);
    let advanced =
        GitHubReviewRefreshStateV1::transition(&read_request, Some(&state), latest).unwrap();
    assert_eq!(
        store
            .compare_and_record(
                &context,
                &read_request,
                Some(&ManifestDigest::new(SHA).unwrap()),
                &advanced,
            )
            .await,
        GitHubReviewRefreshStoreCommitOutcomeV1::Conflict
    );
}
