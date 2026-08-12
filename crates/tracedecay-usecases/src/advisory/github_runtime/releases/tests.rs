use super::*;

fn profile_id(suffix: &str) -> UserProfileId {
    UserProfileId::new(format!("profile.release-reader.{suffix}")).unwrap()
}

fn project_id() -> ProjectId {
    ProjectId::new("project.release-reader").unwrap()
}

fn repository_id() -> RepositoryId {
    RepositoryId::new("repository.release-reader").unwrap()
}

fn target() -> GitHubCiRepositoryTargetV1 {
    GitHubCiRepositoryTargetV1 {
        owner: "ScriptedAlchemy".to_owned(),
        repository: "tracedecay".to_owned(),
    }
}

fn release_json() -> serde_json::Value {
    serde_json::json!([{
        "id": 44,
        "tag_name": "v4.2.0",
        "name": "TraceDecay 4.2.0",
        "html_url": "https://github.com/ScriptedAlchemy/tracedecay/releases/tag/v4.2.0",
        "draft": false,
        "prerelease": false,
        "created_at": "2026-08-12T12:00:00Z",
        "published_at": "2026-08-12T12:01:00Z",
        "assets": [{
            "id": 55,
            "name": "tracedecay-aarch64-apple-darwin.tar.gz",
            "label": null,
            "content_type": "application/gzip",
            "size": 1234,
            "download_count": 9,
            "browser_download_url": "https://github.com/ScriptedAlchemy/tracedecay/releases/download/v4.2.0/tracedecay-aarch64-apple-darwin.tar.gz",
            "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "created_at": "2026-08-12T12:00:30Z",
            "updated_at": "2026-08-12T12:00:31Z"
        }]
    }])
}

#[test]
fn provider_release_page_normalizes_typed_tags_and_assets() {
    let releases = decode_provider_page(
        &serde_json::to_vec(&release_json()).unwrap(),
        &target(),
        &GitHubHttpReadConfigV1::default(),
    )
    .unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].tag.as_str(), "v4.2.0");
    assert_eq!(releases[0].assets[0].asset_id, 55);
    assert_eq!(releases[0].assets[0].size_bytes, 1234);
}

#[test]
fn provider_release_page_rejects_duplicate_asset_identity() {
    let mut value = release_json();
    let duplicate = value[0]["assets"][0].clone();
    value[0]["assets"].as_array_mut().unwrap().push(duplicate);
    assert!(
        decode_provider_page(
            &serde_json::to_vec(&value).unwrap(),
            &target(),
            &GitHubHttpReadConfigV1::default(),
        )
        .is_none()
    );
}

#[test]
fn provider_release_page_rejects_foreign_urls_digests_and_times() {
    for (pointer, invalid) in [
        (
            "/0/html_url",
            serde_json::json!("https://example.com/releases/tag/v4.2.0"),
        ),
        (
            "/0/assets/0/browser_download_url",
            serde_json::json!("https://example.com/tracedecay.tar.gz"),
        ),
        ("/0/assets/0/digest", serde_json::json!("sha256:not-hex")),
        ("/0/created_at", serde_json::json!("not-a-time")),
    ] {
        let mut value = release_json();
        *value.pointer_mut(pointer).unwrap() = invalid;
        assert!(
            decode_provider_page(
                &serde_json::to_vec(&value).unwrap(),
                &target(),
                &GitHubHttpReadConfigV1::default(),
            )
            .is_none(),
            "invalid provider field {pointer} must fail closed",
        );
    }
}

#[test]
fn provider_release_page_accepts_encoded_git_tag_and_asset_names() {
    let mut value = release_json();
    value[0]["tag_name"] = serde_json::json!("release/v4.2.0@rc.1");
    value[0]["html_url"] = serde_json::json!(
        "https://github.com/ScriptedAlchemy/tracedecay/releases/tag/release%2Fv4.2.0@rc.1"
    );
    value[0]["assets"][0]["name"] = serde_json::json!("TraceDecay macOS arm64.tar.gz");
    value[0]["assets"][0]["browser_download_url"] = serde_json::json!(
        "https://github.com/ScriptedAlchemy/tracedecay/releases/download/release%2Fv4.2.0@rc.1/TraceDecay%20macOS%20arm64.tar.gz"
    );
    let releases = decode_provider_page(
        &serde_json::to_vec(&value).unwrap(),
        &target(),
        &GitHubHttpReadConfigV1::default(),
    )
    .unwrap();
    assert_eq!(releases[0].tag.as_str(), "release/v4.2.0@rc.1");
    assert_eq!(releases[0].assets[0].name, "TraceDecay macOS arm64.tar.gz");
}

#[test]
fn continuation_accepts_only_the_exact_https_release_collection() {
    let endpoint = "https://api.github.com/repos/ScriptedAlchemy/tracedecay/releases";
    let mut headers = ureq::http::HeaderMap::new();
    headers.insert(
        "link",
        format!("<{endpoint}?per_page=100&page=2>; rel=\"next\"")
            .parse()
            .unwrap(),
    );
    assert_eq!(
        next_release_page(&headers, "https://api.github.com", endpoint, 1),
        Ok(Some(2))
    );

    headers.insert(
        "link",
        "<https://objects.githubusercontent.com/releases?page=2&per_page=100>; rel=\"next\""
            .parse()
            .unwrap(),
    );
    assert_eq!(
        next_release_page(&headers, "https://api.github.com", endpoint, 1),
        Err(())
    );

    headers.insert(
        "link",
        format!("<{endpoint}?per_page=100&page=21>; rel=\"next\"")
            .parse()
            .unwrap(),
    );
    assert_eq!(
        next_release_page(&headers, "https://api.github.com", endpoint, 20),
        Ok(Some(21))
    );

    headers.insert(
        "link",
        format!("<{endpoint}?per_page=100&page=3>; rel=\"next\"")
            .parse()
            .unwrap(),
    );
    assert_eq!(
        next_release_page(&headers, "https://api.github.com", endpoint, 1),
        Err(())
    );

    headers.insert(
        "link",
        format!(
            "<{endpoint}?per_page=100&page=2>; rel=\"next\", <{endpoint}?per_page=100&page=3>; rel=\"next\""
        )
        .parse()
        .unwrap(),
    );
    assert_eq!(
        next_release_page(&headers, "https://api.github.com", endpoint, 1),
        Err(())
    );
}

#[test]
fn provider_denial_and_rate_limit_are_distinct() {
    let mut headers = ureq::http::HeaderMap::new();
    assert!(matches!(
        classify_status(403, &headers, None),
        ReleaseHttpDispositionV1::Denied
    ));
    assert!(matches!(
        classify_status(429, &headers, None),
        ReleaseHttpDispositionV1::RateLimited { .. }
    ));
    assert!(matches!(
        classify_status(500, &headers, None),
        ReleaseHttpDispositionV1::Unavailable
    ));
    headers.insert("retry-after", "-1".parse().unwrap());
    assert!(retry_at(&headers).is_none());
}

#[test]
fn project_scope_mismatch_is_denied_before_network_access() {
    let profile_id = profile_id("scope-mismatch");
    assert!(super::super::register_profile_github_public_repository_v1(
        profile_id.clone(),
        "ScriptedAlchemy",
        "tracedecay",
    ));
    let ProjectGitHubReleaseAuthorityOpenOutcomeV1::Ready(authority) =
        open_project_github_release_read_authority_v1(
            &profile_id,
            project_id(),
            repository_id(),
            target(),
            GitHubHttpReadConfigV1::default(),
        )
    else {
        panic!("configured public repository must open");
    };
    let outcome = authority.read(
        &ProjectGitHubReleaseReadRequestV1 {
            profile_id: profile_id.clone(),
            project_id: ProjectId::new("project.release-reader.other").unwrap(),
            repository_id: repository_id(),
            max_releases: 20,
        },
        &GitHubReleaseReadControlV1::bounded(Instant::now() + Duration::from_secs(1)),
    );
    assert_eq!(outcome, ProjectGitHubReleaseReadOutcomeV1::Denied);
    assert!(
        super::super::unregister_profile_github_public_repository_v1(
            &profile_id,
            "ScriptedAlchemy",
            "tracedecay",
        )
    );
    let outcome = authority.read(
        &ProjectGitHubReleaseReadRequestV1 {
            profile_id,
            project_id: project_id(),
            repository_id: repository_id(),
            max_releases: 20,
        },
        &GitHubReleaseReadControlV1::bounded(Instant::now() + Duration::from_secs(1)),
    );
    assert_eq!(outcome, ProjectGitHubReleaseReadOutcomeV1::Denied);
}

#[test]
fn unconfigured_and_invalid_repository_access_fail_closed() {
    assert!(matches!(
        open_project_github_release_read_authority_v1(
            &profile_id("unconfigured"),
            project_id(),
            repository_id(),
            target(),
            GitHubHttpReadConfigV1::default(),
        ),
        ProjectGitHubReleaseAuthorityOpenOutcomeV1::Unavailable
    ));
    let invalid_profile = UserProfileId::new("profile.release-reader.invalid").unwrap();
    assert!(matches!(
        open_project_github_release_read_authority_v1(
            &invalid_profile,
            project_id(),
            repository_id(),
            GitHubCiRepositoryTargetV1 {
                owner: "invalid/owner".to_owned(),
                repository: "tracedecay".to_owned(),
            },
            GitHubHttpReadConfigV1::default(),
        ),
        ProjectGitHubReleaseAuthorityOpenOutcomeV1::Unavailable
    ));
}

#[test]
fn release_transport_rejects_http_and_redirect_capable_configuration() {
    let config = GitHubHttpReadConfigV1 {
        rest_base_uri: "http://api.github.com".to_owned(),
        ..GitHubHttpReadConfigV1::default()
    };
    assert!(matches!(
        open_project_github_release_read_authority_v1(
            &profile_id("http-config"),
            project_id(),
            repository_id(),
            target(),
            config,
        ),
        ProjectGitHubReleaseAuthorityOpenOutcomeV1::Unavailable
    ));
}
