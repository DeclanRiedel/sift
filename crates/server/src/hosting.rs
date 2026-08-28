use async_trait::async_trait;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sift_protocol::{
    HostingCheck, HostingCheckState, HostingLink, HostingLinkKind, HostingProviderKind,
    HostingPullRequest, HostingPullRequestState, HostingRepositoryCandidate,
    HostingRepositoryIdentity,
};
use thiserror::Error;

const API_LIMIT: usize = 100;

#[derive(Debug, Error)]
pub enum HostingError {
    #[error("remote is not a supported GitHub, GitLab, or Bitbucket HTTPS repository")]
    UnsupportedRemote,
    #[error("hosting credential is required")]
    CredentialRequired,
    #[error("hosting provider rejected request ({0})")]
    Rejected(u16),
    #[error("hosting provider returned invalid response")]
    InvalidResponse,
    #[error("hosting operation is unavailable for this provider")]
    UnsupportedOperation,
}

pub fn detect_repository(remote: &str) -> Result<HostingRepositoryIdentity, HostingError> {
    let url = Url::parse(remote).map_err(|_| HostingError::UnsupportedRemote)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(HostingError::UnsupportedRemote);
    }
    let host = url.host_str().ok_or(HostingError::UnsupportedRemote)?;
    let provider = match host {
        "github.com" => HostingProviderKind::GitHub,
        "gitlab.com" => HostingProviderKind::GitLab,
        "bitbucket.org" => HostingProviderKind::Bitbucket,
        _ => return Err(HostingError::UnsupportedRemote),
    };
    let mut parts = url
        .path_segments()
        .ok_or(HostingError::UnsupportedRemote)?
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts.len() < 2 || (provider != HostingProviderKind::GitLab && parts.len() != 2) {
        return Err(HostingError::UnsupportedRemote);
    }
    let mut name = parts.pop().ok_or(HostingError::UnsupportedRemote)?;
    if let Some(stripped) = name.strip_suffix(".git") {
        name = stripped.to_owned();
    }
    if !safe_slug(&name) || parts.iter().any(|part| !safe_slug(part)) {
        return Err(HostingError::UnsupportedRemote);
    }
    let owner = parts.join("/");
    Ok(HostingRepositoryIdentity {
        provider,
        host: host.into(),
        web_url: format!("https://{host}/{owner}/{name}"),
        owner,
        name,
    })
}

pub fn browser_links(
    identity: &HostingRepositoryIdentity,
    branch: Option<&str>,
    commit: Option<&str>,
    file: Option<&str>,
) -> Vec<HostingLink> {
    let mut links = vec![HostingLink {
        kind: HostingLinkKind::Repository,
        label: "Repository".into(),
        url: identity.web_url.clone(),
    }];
    if let Some(branch) = branch.filter(|value| validate_ref(value)) {
        links.push(link(identity, HostingLinkKind::Branch, branch, None));
    }
    if let Some(commit) = commit.filter(|value| valid_oid(value)) {
        links.push(link(identity, HostingLinkKind::Commit, commit, None));
        if let Some(file) = file.filter(|value| safe_path(value)) {
            links.push(link(identity, HostingLinkKind::File, commit, Some(file)));
        }
    }
    links
}

fn link(
    identity: &HostingRepositoryIdentity,
    kind: HostingLinkKind,
    revision: &str,
    file: Option<&str>,
) -> HostingLink {
    let mut url = Url::parse(&identity.web_url).expect("validated hosting URL");
    let mut path = url.path_segments_mut().expect("HTTPS URL supports paths");
    match (identity.provider, kind) {
        (HostingProviderKind::GitHub, HostingLinkKind::Branch) => {
            path.push("tree").push(revision);
        }
        (HostingProviderKind::GitHub, HostingLinkKind::Commit) => {
            path.push("commit").push(revision);
        }
        (HostingProviderKind::GitHub, HostingLinkKind::File) => {
            path.push("blob").push(revision);
        }
        (HostingProviderKind::GitLab, HostingLinkKind::Branch) => {
            path.push("-").push("tree").push(revision);
        }
        (HostingProviderKind::GitLab, HostingLinkKind::Commit) => {
            path.push("-").push("commit").push(revision);
        }
        (HostingProviderKind::GitLab, HostingLinkKind::File) => {
            path.push("-").push("blob").push(revision);
        }
        (HostingProviderKind::Bitbucket, HostingLinkKind::Branch) => {
            path.push("src").push(revision);
        }
        (HostingProviderKind::Bitbucket, HostingLinkKind::Commit) => {
            path.push("commits").push(revision);
        }
        (HostingProviderKind::Bitbucket, HostingLinkKind::File) => {
            path.push("src").push(revision);
        }
        _ => {}
    }
    if let Some(file) = file {
        for segment in file.split('/') {
            path.push(segment);
        }
    }
    drop(path);
    HostingLink {
        kind,
        label: match kind {
            HostingLinkKind::Branch => format!("Branch {revision}"),
            HostingLinkKind::Commit => format!("Commit {}", &revision[..8]),
            HostingLinkKind::File => format!("File {}", file.unwrap_or_default()),
            _ => "Open".into(),
        },
        url: url.into(),
    }
}

#[async_trait]
pub trait HostingProvider: Send + Sync {
    async fn repositories(
        &self,
        client: &Client,
        token: &[u8],
    ) -> Result<Vec<HostingRepositoryCandidate>, HostingError>;
    async fn pull_requests(
        &self,
        client: &Client,
        token: Option<&[u8]>,
        identity: &HostingRepositoryIdentity,
        branch: &str,
    ) -> Result<Vec<HostingPullRequest>, HostingError>;
    async fn checks(
        &self,
        client: &Client,
        token: Option<&[u8]>,
        identity: &HostingRepositoryIdentity,
        revision: &str,
    ) -> Result<Vec<HostingCheck>, HostingError>;
    async fn create_pull_request(
        &self,
        client: &Client,
        token: &[u8],
        identity: &HostingRepositoryIdentity,
        draft: PullRequestDraft<'_>,
    ) -> Result<HostingPullRequest, HostingError>;
}

#[derive(Clone, Copy)]
pub struct PullRequestDraft<'a> {
    pub title: &'a str,
    pub body: Option<&'a str>,
    pub head: &'a str,
    pub base: &'a str,
}

pub fn provider(kind: HostingProviderKind) -> &'static dyn HostingProvider {
    match kind {
        HostingProviderKind::GitHub => &GITHUB,
        HostingProviderKind::GitLab | HostingProviderKind::Bitbucket => &LINK_ONLY,
    }
}

struct GitHubProvider;
struct LinkOnlyProvider;
static GITHUB: GitHubProvider = GitHubProvider;
static LINK_ONLY: LinkOnlyProvider = LinkOnlyProvider;

#[derive(Deserialize)]
struct Owner {
    login: String,
}
#[derive(Deserialize)]
struct Repository {
    name: String,
    html_url: String,
    private: bool,
    owner: Owner,
}
#[derive(Deserialize)]
struct Branch {
    #[serde(rename = "ref")]
    name: String,
}
#[derive(Deserialize)]
struct Pull {
    number: u64,
    title: String,
    state: String,
    html_url: String,
    merged_at: Option<String>,
    head: Branch,
    base: Branch,
    user: Option<Owner>,
}
#[derive(Deserialize)]
struct CheckPage {
    check_runs: Vec<CheckRun>,
}
#[derive(Deserialize)]
struct CheckRun {
    name: String,
    status: String,
    conclusion: Option<String>,
    html_url: Option<String>,
}

#[async_trait]
impl HostingProvider for GitHubProvider {
    async fn repositories(
        &self,
        client: &Client,
        token: &[u8],
    ) -> Result<Vec<HostingRepositoryCandidate>, HostingError> {
        let repos: Vec<Repository> = github(
            client
                .get(format!(
                    "https://api.github.com/user/repos?per_page={API_LIMIT}&sort=updated"
                ))
                .bearer_auth(token_text(token)?),
        )
        .await?;
        Ok(repos
            .into_iter()
            .take(API_LIMIT)
            .map(|repo| HostingRepositoryCandidate {
                identity: HostingRepositoryIdentity {
                    provider: HostingProviderKind::GitHub,
                    host: "github.com".into(),
                    owner: repo.owner.login,
                    name: repo.name,
                    web_url: repo.html_url,
                },
                private: repo.private,
            })
            .collect())
    }

    async fn pull_requests(
        &self,
        client: &Client,
        token: Option<&[u8]>,
        identity: &HostingRepositoryIdentity,
        branch: &str,
    ) -> Result<Vec<HostingPullRequest>, HostingError> {
        let mut url = Url::parse(&format!(
            "https://api.github.com/repos/{}/{}/pulls",
            identity.owner, identity.name
        ))
        .map_err(|_| HostingError::InvalidResponse)?;
        url.query_pairs_mut()
            .append_pair("state", "all")
            .append_pair("per_page", "20")
            .append_pair("head", &format!("{}:{branch}", identity.owner));
        let mut request = client.get(url);
        if let Some(token) = token {
            request = request.bearer_auth(token_text(token)?);
        }
        let pulls: Vec<Pull> = github(request).await?;
        Ok(pulls.into_iter().map(map_pull).collect())
    }

    async fn checks(
        &self,
        client: &Client,
        token: Option<&[u8]>,
        identity: &HostingRepositoryIdentity,
        revision: &str,
    ) -> Result<Vec<HostingCheck>, HostingError> {
        let mut request = client.get(format!("https://api.github.com/repos/{}/{}/commits/{revision}/check-runs?per_page={API_LIMIT}", identity.owner, identity.name)).header("Accept", "application/vnd.github+json");
        if let Some(token) = token {
            request = request.bearer_auth(token_text(token)?);
        }
        let page: CheckPage = github(request).await?;
        Ok(page
            .check_runs
            .into_iter()
            .take(API_LIMIT)
            .map(|check| HostingCheck {
                name: check.name,
                state: map_check(&check.status, check.conclusion.as_deref()),
                url: check.html_url,
                description: check.conclusion,
            })
            .collect())
    }

    async fn create_pull_request(
        &self,
        client: &Client,
        token: &[u8],
        identity: &HostingRepositoryIdentity,
        draft: PullRequestDraft<'_>,
    ) -> Result<HostingPullRequest, HostingError> {
        #[derive(Serialize)]
        struct Request<'a> {
            title: &'a str,
            body: Option<&'a str>,
            head: &'a str,
            base: &'a str,
        }
        let pull: Pull = github(
            client
                .post(format!(
                    "https://api.github.com/repos/{}/{}/pulls",
                    identity.owner, identity.name
                ))
                .bearer_auth(token_text(token)?)
                .json(&Request {
                    title: draft.title,
                    body: draft.body,
                    head: draft.head,
                    base: draft.base,
                }),
        )
        .await?;
        Ok(map_pull(pull))
    }
}

#[async_trait]
impl HostingProvider for LinkOnlyProvider {
    async fn repositories(
        &self,
        _: &Client,
        _: &[u8],
    ) -> Result<Vec<HostingRepositoryCandidate>, HostingError> {
        Err(HostingError::UnsupportedOperation)
    }
    async fn pull_requests(
        &self,
        _: &Client,
        _: Option<&[u8]>,
        _: &HostingRepositoryIdentity,
        _: &str,
    ) -> Result<Vec<HostingPullRequest>, HostingError> {
        Ok(Vec::new())
    }
    async fn checks(
        &self,
        _: &Client,
        _: Option<&[u8]>,
        _: &HostingRepositoryIdentity,
        _: &str,
    ) -> Result<Vec<HostingCheck>, HostingError> {
        Ok(Vec::new())
    }
    async fn create_pull_request(
        &self,
        _: &Client,
        _: &[u8],
        _: &HostingRepositoryIdentity,
        _: PullRequestDraft<'_>,
    ) -> Result<HostingPullRequest, HostingError> {
        Err(HostingError::UnsupportedOperation)
    }
}

async fn github<T: for<'de> Deserialize<'de>>(
    request: reqwest::RequestBuilder,
) -> Result<T, HostingError> {
    let response = request
        .header("User-Agent", "sift-hosting-integration")
        .send()
        .await
        .map_err(|_| HostingError::InvalidResponse)?;
    if !response.status().is_success() {
        return Err(HostingError::Rejected(response.status().as_u16()));
    }
    response
        .json()
        .await
        .map_err(|_| HostingError::InvalidResponse)
}

fn token_text(token: &[u8]) -> Result<&str, HostingError> {
    std::str::from_utf8(token).map_err(|_| HostingError::CredentialRequired)
}
fn map_pull(pull: Pull) -> HostingPullRequest {
    HostingPullRequest {
        id: pull.number,
        title: pull.title,
        state: if pull.merged_at.is_some() {
            HostingPullRequestState::Merged
        } else if pull.state == "open" {
            HostingPullRequestState::Open
        } else {
            HostingPullRequestState::Closed
        },
        url: pull.html_url,
        head_branch: pull.head.name,
        base_branch: pull.base.name,
        author: pull.user.map(|user| user.login),
    }
}
fn map_check(status: &str, conclusion: Option<&str>) -> HostingCheckState {
    if status != "completed" {
        return HostingCheckState::Pending;
    }
    match conclusion {
        Some("success") => HostingCheckState::Success,
        Some("failure" | "timed_out" | "cancelled" | "action_required") => {
            HostingCheckState::Failure
        }
        Some("neutral") => HostingCheckState::Neutral,
        Some("skipped") => HostingCheckState::Skipped,
        _ => HostingCheckState::Unknown,
    }
}
fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
pub fn validate_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('-')
        && !value.contains("..")
        && !value.contains(['~', '^', ':', '?', '*', '[', '\\'])
        && !value.bytes().any(|byte| byte.is_ascii_control())
}
fn safe_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && value != "."
        && value != ".."
}
fn safe_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.starts_with('/')
        && !value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_supported_https_remotes_without_credentials() {
        let github = detect_repository("https://github.com/sift-org/sift.git").unwrap();
        assert_eq!(github.provider, HostingProviderKind::GitHub);
        assert_eq!(
            (github.owner.as_str(), github.name.as_str()),
            ("sift-org", "sift")
        );
        assert_eq!(
            detect_repository("https://gitlab.com/group/team/project.git")
                .unwrap()
                .owner,
            "group/team"
        );
        assert!(detect_repository("https://token@github.com/org/repo.git").is_err());
        assert!(detect_repository("git@github.com:org/repo.git").is_err());
    }
    #[test]
    fn links_encode_refs_and_confine_paths() {
        let identity = detect_repository("https://github.com/org/repo.git").unwrap();
        let links = browser_links(
            &identity,
            Some("feature/a b"),
            Some("0123456789012345678901234567890123456789"),
            Some("sql/a migration.sql"),
        );
        assert_eq!(links.len(), 4);
        assert!(links[1].url.contains("feature%2Fa%20b"));
        assert!(links[3].url.ends_with("sql/a%20migration.sql"));
        assert_eq!(
            browser_links(&identity, None, None, Some("../secret")).len(),
            1
        );
    }
}
