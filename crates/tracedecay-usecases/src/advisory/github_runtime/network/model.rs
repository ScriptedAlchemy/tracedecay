use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubRepositoryTargetV1 {
    pub owner: String,
    pub repository: String,
    pub pull_request_number: u64,
    pub pull_request_id: tracedecay_domain::feedback::GitHubPullRequestIdV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubCiRepositoryTargetV1 {
    pub owner: String,
    pub repository: String,
}

impl GitHubCiRepositoryTargetV1 {
    pub fn validate(&self) -> bool {
        valid_path_segment(&self.owner) && valid_path_segment(&self.repository)
    }
}

impl GitHubRepositoryTargetV1 {
    pub fn validate(&self) -> bool {
        valid_path_segment(&self.owner)
            && valid_path_segment(&self.repository)
            && self.pull_request_number > 0
            && i32::try_from(self.pull_request_number).is_ok()
            && self.pull_request_id.validate().is_ok()
    }
}

#[derive(Clone, Debug)]
pub struct GitHubHttpReadConfigV1 {
    pub rest_base_uri: String,
    pub graphql_uri: String,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub socket_timeout: Duration,
}

impl Default for GitHubHttpReadConfigV1 {
    fn default() -> Self {
        Self {
            rest_base_uri: "https://api.github.com".to_owned(),
            graphql_uri: "https://api.github.com/graphql".to_owned(),
            request_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            socket_timeout: Duration::from_secs(20),
        }
    }
}

impl GitHubHttpReadConfigV1 {
    pub(super) fn validate(&self) -> bool {
        let (Ok(rest), Ok(graphql)) = (
            Url::parse(&self.rest_base_uri),
            Url::parse(&self.graphql_uri),
        ) else {
            return false;
        };
        rest.scheme() == "https"
            && graphql.scheme() == "https"
            && rest.host_str() == graphql.host_str()
            && rest.port_or_known_default() == graphql.port_or_known_default()
            && !self.request_timeout.is_zero()
            && !self.connect_timeout.is_zero()
            && !self.socket_timeout.is_zero()
    }
}
