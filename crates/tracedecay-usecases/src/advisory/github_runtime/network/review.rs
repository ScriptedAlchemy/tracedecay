use super::*;

#[derive(Clone)]
pub struct GitHubReadOnlyClientV1 {
    pub(super) agent: ureq::Agent,
    pub(super) target: GitHubRepositoryTargetV1,
    pub(super) credential: GitHubReadOnlyCredentialV1,
    pub(super) config: GitHubHttpReadConfigV1,
}

impl GitHubReadOnlyClientV1 {
    pub fn new(
        target: GitHubRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<Self> {
        if matches!(
            credential.authorization_for_target(&target, GitHubReadPermissionV1::PullRequests),
            GitHubCredentialAuthorizationV1::Denied
        ) {
            return None;
        }
        Self::build(target, credential, config)
    }

    pub fn new_for_ci(
        target: GitHubCiRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<GitHubCiReadOnlyClientV1> {
        GitHubCiReadOnlyClientV1::new(target, credential, config)
    }

    pub(super) fn build(
        target: GitHubRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<Self> {
        if !target.validate() || !config.validate() {
            return None;
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(config.request_timeout))
            .timeout_connect(Some(config.connect_timeout))
            .timeout_recv_response(Some(config.socket_timeout))
            .timeout_recv_body(Some(config.socket_timeout))
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        Some(Self {
            agent,
            target,
            credential,
            config,
        })
    }

    pub(super) fn execute_rest(
        &self,
        context: &RequestContext,
        request: &GitHubRestReadRequestV1,
    ) -> GitHubReadNetworkOutcomeV1 {
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
            || request.pull_request_id != self.target.pull_request_id
        {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        let Some(page) = page_from_cursor(request.resume.cursor.as_ref()) else {
            return GitHubReadNetworkOutcomeV1::Unavailable;
        };
        let suffix = match request.descriptor.operation {
            GitHubReviewReadOperationV1::RestGetPullRequest => String::new(),
            GitHubReviewReadOperationV1::RestListPullRequestReviews => {
                format!("/reviews?per_page=100&page={page}")
            }
            GitHubReviewReadOperationV1::RestListPullRequestReviewComments => {
                format!("/comments?per_page=100&page={page}")
            }
            GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads => {
                return GitHubReadNetworkOutcomeV1::Unavailable;
            }
        };
        let url = format!(
            "{}/repos/{}/{}/pulls/{}{}",
            self.config.rest_base_uri.trim_end_matches('/'),
            self.target.owner,
            self.target.repository,
            self.target.pull_request_number,
            suffix
        );
        if !request_context_admitted(context) {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        let response = self.get(
            &url,
            (page == 1)
                .then_some(request.resume.etag.as_ref())
                .flatten(),
            GitHubReadPermissionV1::PullRequests,
        );
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        Self::decode_rest_response(response, request.descriptor.operation, page)
    }

    pub(super) fn execute_graphql(
        &self,
        context: &RequestContext,
        request: &GitHubGraphQlReadRequestV1,
    ) -> GitHubReadNetworkOutcomeV1 {
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
            || request.pull_request_id != self.target.pull_request_id
            || request
                .resume
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.as_str().starts_with("rest-page:"))
        {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        let variables = json!({
            "owner": self.target.owner,
            "repository": self.target.repository,
            "number": self.target.pull_request_number,
            "threadAfter": request.resume.cursor.as_ref().map(GitHubReviewCursorV1::as_str),
            "commentThreadId": "unused",
            "commentAfter": null,
            "loadThreads": true,
            "loadComments": false,
        });
        let (mut envelope, mut rate_limit) = match self.graphql(context, &variables) {
            Ok(page) => page,
            Err(failure) => return network_failure(failure),
        };
        if !envelope.errors.is_empty() {
            if let Some(checkpoint) = rate_limit
                .as_ref()
                .filter(|checkpoint| checkpoint.remaining == 0)
                .cloned()
            {
                return network_failure(HttpResponseV1::RateLimited {
                    checkpoint: Some(checkpoint),
                    retry_at: None,
                });
            }
            return GitHubReadNetworkOutcomeV1::Unavailable;
        }
        if let Err(failure) =
            self.complete_nested_comment_pages(context, &mut envelope, &mut rate_limit)
        {
            return network_failure(failure);
        }
        let next_cursor = envelope
            .data
            .as_ref()
            .and_then(|data| data.repository.as_ref())
            .and_then(|repository| repository.pull_request.as_ref())
            .and_then(|pull_request| {
                pull_request
                    .review_threads
                    .page_info
                    .has_next_page
                    .then_some(pull_request.review_threads.page_info.end_cursor.as_deref())
                    .flatten()
            })
            .and_then(|cursor| GitHubReviewCursorV1::new(cursor).ok());
        let Ok(body) = serde_json::to_vec(&envelope) else {
            return GitHubReadNetworkOutcomeV1::Unavailable;
        };
        if body.len() > MAX_GITHUB_READ_RESPONSE_BYTES_V1 {
            return GitHubReadNetworkOutcomeV1::Unavailable;
        }
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
            metadata: GitHubReadNetworkMetadataV1 {
                status: GitHubReadNetworkStatusV1::Ok,
                etag: None,
                next_cursor,
                rate_limit,
                retry_at: None,
            },
            body,
        })
    }

    pub(super) fn complete_nested_comment_pages(
        &self,
        context: &RequestContext,
        envelope: &mut GraphQlResponseV1,
        rate_limit: &mut Option<GitHubReviewRateLimitCheckpointV1>,
    ) -> Result<(), HttpResponseV1> {
        let Some(threads) = envelope
            .data
            .as_mut()
            .and_then(|data| data.repository.as_mut())
            .and_then(|repository| repository.pull_request.as_mut())
            .map(|pull_request| &mut pull_request.review_threads.nodes)
        else {
            return Err(HttpResponseV1::Unavailable);
        };
        if threads.len() > 100 {
            return Err(HttpResponseV1::Unavailable);
        }
        let mut total = threads
            .iter()
            .map(|thread| thread.comments.nodes.len())
            .sum::<usize>();
        if total > MAX_REVIEW_ITEMS_V1 {
            return Err(HttpResponseV1::Unavailable);
        }
        for thread in threads {
            let mut pages = 0_usize;
            while thread.comments.page_info.has_next_page {
                pages += 1;
                if pages > MAX_NESTED_COMMENT_PAGES_V1 || total >= MAX_REVIEW_ITEMS_V1 {
                    return Err(HttpResponseV1::Unavailable);
                }
                let Some(comment_after) = thread.comments.page_info.end_cursor.clone() else {
                    return Err(HttpResponseV1::Unavailable);
                };
                let variables = json!({
                    "owner": self.target.owner,
                    "repository": self.target.repository,
                    "number": self.target.pull_request_number,
                    "threadAfter": null,
                    "commentThreadId": thread.id.clone(),
                    "commentAfter": comment_after,
                    "loadThreads": false,
                    "loadComments": true,
                });
                let (page, page_rate_limit) = self.graphql(context, &variables)?;
                merge_rate_limit(rate_limit, page_rate_limit);
                if !page.errors.is_empty() {
                    if let Some(checkpoint) = rate_limit
                        .as_ref()
                        .filter(|checkpoint| checkpoint.remaining == 0)
                        .cloned()
                    {
                        return Err(HttpResponseV1::RateLimited {
                            checkpoint: Some(checkpoint),
                            retry_at: None,
                        });
                    }
                    return Err(HttpResponseV1::Unavailable);
                }
                let Some(GraphQlCommentPageNodeV1 { id, comments }) =
                    page.data.and_then(|data| data.node)
                else {
                    return Err(HttpResponseV1::Unavailable);
                };
                if id != thread.id || comments.nodes.is_empty() {
                    return Err(HttpResponseV1::Unavailable);
                }
                total = total.saturating_add(comments.nodes.len());
                if total > MAX_REVIEW_ITEMS_V1 {
                    return Err(HttpResponseV1::Unavailable);
                }
                thread.comments.nodes.extend(comments.nodes);
                thread.comments.page_info = comments.page_info;
            }
        }
        Ok(())
    }

    pub(super) fn graphql(
        &self,
        context: &RequestContext,
        variables: &serde_json::Value,
    ) -> Result<(GraphQlResponseV1, Option<GitHubReviewRateLimitCheckpointV1>), HttpResponseV1>
    {
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return Err(HttpResponseV1::Denied);
        }
        let payload = json!({
            "query": GITHUB_REVIEW_THREADS_QUERY_V1,
            "variables": variables,
        });
        let response = self.post_static_graphql(&payload);
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return Err(HttpResponseV1::Denied);
        }
        match response {
            HttpResponseV1::Ok {
                body, rate_limit, ..
            } => serde_json::from_slice(&body)
                .map(|envelope| (envelope, rate_limit))
                .map_err(|_| HttpResponseV1::Unavailable),
            failure => Err(failure),
        }
    }

    pub(super) fn decode_rest_response(
        response: HttpResponseV1,
        operation: GitHubReviewReadOperationV1,
        current_page: u32,
    ) -> GitHubReadNetworkOutcomeV1 {
        match response {
            HttpResponseV1::Ok {
                body,
                etag,
                next_page,
                rate_limit,
            } => {
                let (valid, item_count) = match operation {
                    GitHubReviewReadOperationV1::RestGetPullRequest => (
                        parse_bounded::<RestPullRequestV1>(&body).is_some() && next_page.is_none(),
                        None,
                    ),
                    GitHubReviewReadOperationV1::RestListPullRequestReviews => {
                        match parse_bounded::<Vec<RestReviewV1>>(&body) {
                            Some(items) if items.len() <= 100 => (true, Some(items.len())),
                            _ => (false, None),
                        }
                    }
                    GitHubReviewReadOperationV1::RestListPullRequestReviewComments => {
                        match parse_bounded::<Vec<RestReviewCommentV1>>(&body) {
                            Some(items) if items.len() <= 100 => (true, Some(items.len())),
                            _ => (false, None),
                        }
                    }
                    GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads => {
                        (false, None)
                    }
                };
                if !valid {
                    return GitHubReadNetworkOutcomeV1::Unavailable;
                }
                let next_page = match next_page {
                    Some(next)
                        if next == current_page.saturating_add(1)
                            && next <= MAX_REVIEW_SCAN_PAGES_V1 =>
                    {
                        Some(next)
                    }
                    Some(_) => return GitHubReadNetworkOutcomeV1::Unavailable,
                    None if item_count == Some(100) => {
                        let Some(next) = current_page.checked_add(1) else {
                            return GitHubReadNetworkOutcomeV1::Unavailable;
                        };
                        if next > MAX_REVIEW_SCAN_PAGES_V1 {
                            return GitHubReadNetworkOutcomeV1::Unavailable;
                        }
                        Some(next)
                    }
                    None => None,
                };
                GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::Ok,
                        etag,
                        next_cursor: next_page.and_then(|page| {
                            GitHubReviewCursorV1::new(format!("rest-page:{page}")).ok()
                        }),
                        rate_limit,
                        retry_at: None,
                    },
                    body,
                })
            }
            HttpResponseV1::NotModified { etag, rate_limit } => {
                GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::NotModified,
                        etag,
                        next_cursor: None,
                        rate_limit,
                        retry_at: None,
                    },
                    body: Vec::new(),
                })
            }
            HttpResponseV1::RateLimited {
                checkpoint,
                retry_at,
            } => GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                metadata: GitHubReadNetworkMetadataV1 {
                    status: GitHubReadNetworkStatusV1::RateLimited,
                    etag: None,
                    next_cursor: None,
                    rate_limit: checkpoint,
                    retry_at,
                },
                body: Vec::new(),
            }),
            HttpResponseV1::Denied => GitHubReadNetworkOutcomeV1::Denied,
            HttpResponseV1::Unavailable => GitHubReadNetworkOutcomeV1::Unavailable,
        }
    }

    pub(super) fn get(
        &self,
        url: &str,
        etag: Option<&GitHubReviewEtagV1>,
        permission: GitHubReadPermissionV1,
    ) -> HttpResponseV1 {
        let authorization = self
            .credential
            .authorization_for_target(&self.target, permission);
        if matches!(&authorization, GitHubCredentialAuthorizationV1::Denied) {
            return HttpResponseV1::Denied;
        }
        let mut request = self
            .agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tracedecay-github-read");
        if let GitHubCredentialAuthorizationV1::Private(authorization) = &authorization {
            request = request.header("Authorization", authorization.as_str());
        }
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag.as_str());
        }
        decode_ureq_response(request.call(), MAX_GITHUB_READ_RESPONSE_BYTES_V1)
    }

    pub(super) fn post_static_graphql(&self, payload: &serde_json::Value) -> HttpResponseV1 {
        let authorization = self
            .credential
            .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests);
        if matches!(&authorization, GitHubCredentialAuthorizationV1::Denied) {
            return HttpResponseV1::Denied;
        }
        let mut request = self
            .agent
            .post(&self.config.graphql_uri)
            .header("Accept", "application/json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tracedecay-github-read");
        if let GitHubCredentialAuthorizationV1::Private(authorization) = &authorization {
            request = request.header("Authorization", authorization.as_str());
        }
        decode_ureq_response(
            request.send_json(payload),
            MAX_GITHUB_READ_RESPONSE_BYTES_V1,
        )
    }
}

impl GitHubReadOnlyNetworkAuthorityV1 for GitHubReadOnlyClientV1 {
    fn get<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubRestReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1> {
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return Box::pin(async { GitHubReadNetworkOutcomeV1::Denied });
        }
        let client = self.clone();
        let context = context.clone();
        let request = request.clone();
        Box::pin(async move {
            let wait_context = context.clone();
            let task = tokio::task::spawn_blocking(move || client.execute_rest(&context, &request));
            wait_for_read(&wait_context, task)
                .await
                .unwrap_or(GitHubReadNetworkOutcomeV1::Unavailable)
        })
    }

    fn query<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubGraphQlReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1> {
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return Box::pin(async { GitHubReadNetworkOutcomeV1::Denied });
        }
        let client = self.clone();
        let context = context.clone();
        let request = request.clone();
        Box::pin(async move {
            let wait_context = context.clone();
            let task =
                tokio::task::spawn_blocking(move || client.execute_graphql(&context, &request));
            wait_for_read(&wait_context, task)
                .await
                .unwrap_or(GitHubReadNetworkOutcomeV1::Unavailable)
        })
    }
}
