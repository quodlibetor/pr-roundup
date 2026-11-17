//! Reading pull requests from GitHub's GraphQL API.

use color_eyre::{
    Result, Section,
    eyre::{WrapErr, eyre},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tracing::warn;

use crate::model::{Pr, StackEntry, Status};

const PR_FRAGMENT: &str = "
fragment Pr on PullRequest {
  id number title url state isDraft baseRefName headRefName updatedAt
  repository { nameWithOwner }
  reviewDecision
  stackEntry { position stack { number } }
  reviews(last: 20) { nodes { state } }
}";

const SEARCH: &str = "
query($q: String!, $after: String) {
  search(query: $q, type: ISSUE, first: 100, after: $after) {
    pageInfo { hasNextPage endCursor }
    nodes { __typename ...Pr }
  }
}";

const NODES: &str = "
query($ids: [ID!]!) {
  nodes(ids: $ids) { __typename ...Pr }
}";

#[derive(Clone)]
pub struct GitHub {
    http: reqwest::Client,
    token: String,
}

impl GitHub {
    pub async fn new() -> Result<GitHub> {
        let http = reqwest::Client::builder()
            .user_agent("pr-roundup")
            .build()?;
        Ok(GitHub {
            http,
            token: token().await?,
        })
    }

    async fn query<T: DeserializeOwned>(&self, query: &str, variables: Value) -> Result<T> {
        #[derive(Deserialize)]
        struct Response<T> {
            data: Option<T>,
            #[serde(default)]
            errors: Vec<Value>,
        }

        let response = self
            .http
            .post("https://api.github.com/graphql")
            .bearer_auth(&self.token)
            .json(&json!({ "query": format!("{query}\n{PR_FRAGMENT}"), "variables": variables }))
            .send()
            .await
            .wrap_err("sending a GraphQL query to GitHub")?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(eyre!("GitHub rejected the token"))
                .suggestion("run `gh auth login`, or update GITHUB_TOKEN");
        }
        let body: Response<T> = response
            .error_for_status()?
            .json()
            .await
            .wrap_err("reading GitHub's GraphQL response")?;
        match body.data {
            Some(data) => {
                // A node that no longer exists is reported as an error next to
                // a null in the data; the rest of the answer is still good.
                if !body.errors.is_empty() {
                    warn!(errors = ?body.errors, "GitHub answered with errors");
                }
                Ok(data)
            }
            None => Err(eyre!("GitHub answered with errors: {:?}", body.errors)),
        }
    }

    /// Every open pull request each search finds, without duplicates.
    pub async fn search(&self, searches: &[String]) -> Result<Vec<Pr>> {
        #[derive(Deserialize)]
        struct Data {
            search: Search,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Search {
            page_info: PageInfo,
            nodes: Vec<Option<Value>>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PageInfo {
            has_next_page: bool,
            end_cursor: Option<String>,
        }

        let mut prs: Vec<Pr> = Vec::new();
        for q in searches {
            let mut after: Option<String> = None;
            loop {
                let data: Data = self
                    .query(SEARCH, json!({ "q": q, "after": after }))
                    .await
                    .wrap_err_with(|| format!("searching for `{q}`"))?;
                for pr in data.search.nodes.into_iter().filter_map(pull_request) {
                    if !prs.iter().any(|seen| seen.id == pr.id) {
                        prs.push(pr);
                    }
                }
                match data.search.page_info {
                    PageInfo {
                        has_next_page: true,
                        end_cursor: Some(cursor),
                    } => after = Some(cursor),
                    _ => break,
                }
            }
        }
        Ok(prs)
    }

    /// The pull requests with these node ids, whatever state they are in.
    pub async fn by_id(&self, ids: &[String]) -> Result<Vec<Pr>> {
        #[derive(Deserialize)]
        struct Data {
            nodes: Vec<Option<Value>>,
        }

        let mut prs = Vec::new();
        for chunk in ids.chunks(100) {
            let data: Data = self
                .query(NODES, json!({ "ids": chunk }))
                .await
                .wrap_err("looking up tagged pull requests")?;
            prs.extend(data.nodes.into_iter().filter_map(pull_request));
        }
        Ok(prs)
    }
}

/// Turn a search or node result into a pull request, passing over issues and
/// anything else that isn't one.
fn pull_request(node: Option<Value>) -> Option<Pr> {
    let node = node?;
    if node.get("__typename").and_then(Value::as_str) != Some("PullRequest") {
        return None;
    }
    match serde_json::from_value::<RawPr>(node) {
        Ok(raw) => Some(raw.into()),
        Err(error) => {
            warn!(%error, "could not read a pull request from GitHub's answer");
            None
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPr {
    id: String,
    number: u64,
    title: String,
    url: String,
    state: String,
    is_draft: bool,
    base_ref_name: String,
    head_ref_name: String,
    updated_at: String,
    repository: RawRepository,
    review_decision: Option<String>,
    stack_entry: Option<RawStackEntry>,
    reviews: Option<RawReviews>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRepository {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct RawStackEntry {
    position: u64,
    stack: RawStack,
}

#[derive(Deserialize)]
struct RawStack {
    number: u64,
}

#[derive(Deserialize)]
struct RawReviews {
    nodes: Vec<Option<RawReview>>,
}

#[derive(Deserialize)]
struct RawReview {
    state: String,
}

impl From<RawPr> for Pr {
    fn from(raw: RawPr) -> Pr {
        let status = match (raw.state.as_str(), raw.review_decision.as_deref()) {
            ("MERGED", _) => Status::Merged,
            ("CLOSED", _) => Status::Closed,
            (_, Some("APPROVED")) => Status::Approved,
            (_, Some("CHANGES_REQUESTED")) => Status::ChangesRequested,
            _ => {
                // reviewDecision has no verdict for a review that only
                // commented, so look at the reviews themselves, as `jj spr
                // list` does.
                let commented = raw
                    .reviews
                    .iter()
                    .flat_map(|reviews| reviews.nodes.iter().flatten())
                    .any(|review| review.state == "COMMENTED");
                if commented {
                    Status::Commented
                } else {
                    Status::Pending
                }
            }
        };
        Pr {
            id: raw.id,
            repo: raw.repository.name_with_owner,
            number: raw.number,
            title: raw.title,
            url: raw.url,
            status,
            draft: raw.is_draft,
            base: raw.base_ref_name,
            head: raw.head_ref_name,
            updated_at: raw.updated_at,
            stack: raw.stack_entry.map(|entry| StackEntry {
                stack: entry.stack.number,
                position: entry.position,
            }),
        }
    }
}

/// `$GITHUB_TOKEN` if set, otherwise `gh auth token`.
async fn token() -> Result<String> {
    if let Ok(token) = std::env::var("GITHUB_TOKEN")
        && !token.trim().is_empty()
    {
        return Ok(token.trim().to_string());
    }
    let output = tokio::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .await
        .wrap_err("running `gh auth token`")
        .suggestion("install the GitHub CLI, or set GITHUB_TOKEN")?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || token.is_empty() {
        return Err(eyre!("`gh auth token` returned no token"))
            .suggestion("run `gh auth login`, or set GITHUB_TOKEN");
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(state: &str, decision: Option<&str>, reviews: &[&str]) -> Value {
        json!({
            "__typename": "PullRequest",
            "id": "PR_1", "number": 1, "title": "t", "url": "u",
            "state": state, "isDraft": false,
            "baseRefName": "main", "headRefName": "h", "updatedAt": "2026-01-01T00:00:00Z",
            "repository": { "nameWithOwner": "o/r" },
            "reviewDecision": decision,
            "stackEntry": { "position": 2, "stack": { "number": 7 } },
            "reviews": { "nodes": reviews.iter().map(|s| json!({ "state": s })).collect::<Vec<_>>() },
        })
    }

    #[test]
    fn a_merged_pull_request_is_merged_whatever_its_reviews_said() {
        let pr = pull_request(Some(node("MERGED", Some("CHANGES_REQUESTED"), &[]))).unwrap();
        assert_eq!(pr.status, Status::Merged);
        assert_eq!(
            pr.stack,
            Some(StackEntry {
                stack: 7,
                position: 2
            })
        );
    }

    #[test]
    fn a_comment_without_a_verdict_counts_as_commented() {
        let pr = pull_request(Some(node("OPEN", Some("REVIEW_REQUIRED"), &["COMMENTED"]))).unwrap();
        assert_eq!(pr.status, Status::Commented);
        let pr = pull_request(Some(node("OPEN", None, &[]))).unwrap();
        assert_eq!(pr.status, Status::Pending);
    }

    #[test]
    fn issues_are_passed_over() {
        assert_eq!(pull_request(Some(json!({ "__typename": "Issue" }))), None);
    }
}
