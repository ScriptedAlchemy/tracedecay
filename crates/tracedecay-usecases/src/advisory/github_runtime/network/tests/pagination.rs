use super::*;

#[test]
fn full_rest_page_without_link_continues_bounded_scan() {
    let body = serde_json::to_vec(
        &(1..=100)
            .map(|id| RestReviewV1 {
                id,
                node_id: None,
                state: None,
                commit_id: None,
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let outcome = GitHubReadOnlyClientV1::decode_rest_response(
        HttpResponseV1::Ok {
            body,
            etag: None,
            next_page: None,
            rate_limit: None,
        },
        GitHubReviewReadOperationV1::RestListPullRequestReviews,
        1,
    );
    let GitHubReadNetworkOutcomeV1::Response(response) = outcome else {
        panic!("full page must continue");
    };
    assert_eq!(
        response.metadata.next_cursor.unwrap().as_str(),
        "rest-page:2"
    );
    assert!(page_from_cursor(GitHubReviewCursorV1::new("rest-page:21").ok().as_ref()).is_none());
}
