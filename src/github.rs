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
  author { login }
  reviews(last: 20) { nodes { state submittedAt author { __typename login } } }
  comments(last: 20) { nodes { createdAt author { __typename login } } }
  commits(last: 1) { nodes { commit { committedDate } } }
  timelineItems(last: 1, itemTypes: [HEAD_REF_FORCE_PUSHED_EVENT]) {
    nodes { ... on HeadRefForcePushedEvent { createdAt } }
  }
}";

/// A page of a search, along with pull requests looked up by id, so that
/// one request can do both.
const SEARCH: &str = "
query($q: String!, $after: String, $ids: [ID!]!) {
  search(query: $q, type: ISSUE, first: 100, after: $after) {
    pageInfo { hasNextPage endCursor }
    nodes { __typename ...Pr }
  }
  nodes(ids: $ids) { __typename ...Pr }
}";

const NODES: &str = "
query($ids: [ID!]!) {
  nodes(ids: $ids) { __typename ...Pr }
}";

#[derive(Clone)]
pub struct GitHub {
    http: reqwest::Client,
    token: String,
    /// Logins whose reviews and comments aren't feedback.
    ignore_authors: Vec<String>,
}

impl GitHub {
    pub async fn new(ignore_authors: Vec<String>) -> Result<GitHub> {
        let http = reqwest::Client::builder()
            .user_agent("pr-roundup")
            .build()?;
        Ok(GitHub {
            http,
            token: token().await?,
            ignore_authors,
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
    /// Every pull request the searches find, and those with the ids in
    /// `ids` whatever state they are in, without duplicates. The first
    /// request also looks up the first hundred of `ids`.
    pub async fn pull_requests(&self, searches: &[String], ids: &[String]) -> Result<Vec<Pr>> {
        #[derive(Deserialize)]
        struct Data {
            search: Search,
            #[serde(default)]
            nodes: Vec<Option<Value>>,
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

        let (first_ids, later_ids) = ids.split_at(ids.len().min(100));
        let mut ids = first_ids;
        let mut looked_up: Vec<Pr> = Vec::new();
        let mut prs: Vec<Pr> = Vec::new();
        for q in searches {
            let mut after: Option<String> = None;
            loop {
                let data: Data = self
                    .query(SEARCH, json!({ "q": q, "after": after, "ids": ids }))
                    .await
                    .wrap_err_with(|| format!("searching for `{q}`"))?;
                ids = &[];
                looked_up.extend(
                    data.nodes
                        .into_iter()
                        .filter_map(|node| pull_request(node, &self.ignore_authors)),
                );
                for pr in data
                    .search
                    .nodes
                    .into_iter()
                    .filter_map(|node| pull_request(node, &self.ignore_authors))
                {
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
        looked_up.extend(self.by_id(later_ids).await?);
        for pr in looked_up {
            if !prs.iter().any(|seen| seen.id == pr.id) {
                prs.push(pr);
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
            prs.extend(
                data.nodes
                    .into_iter()
                    .filter_map(|node| pull_request(node, &self.ignore_authors)),
            );
        }
        Ok(prs)
    }
}

/// Turn a search or node result into a pull request, passing over issues and
/// anything else that isn't one. Reviews and comments by `ignore_authors`
/// aren't feedback.
fn pull_request(node: Option<Value>, ignore_authors: &[String]) -> Option<Pr> {
    let node = node?;
    if node.get("__typename").and_then(Value::as_str) != Some("PullRequest") {
        return None;
    }
    match serde_json::from_value::<RawPr>(node) {
        Ok(raw) => Some(raw.into_pr(ignore_authors)),
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
    author: Option<RawAuthor>,
    reviews: Option<Nodes<RawReview>>,
    comments: Option<Nodes<RawComment>>,
    commits: Option<Nodes<RawCommitNode>>,
    timeline_items: Option<Nodes<RawForcePush>>,
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
struct Nodes<T> {
    nodes: Vec<Option<T>>,
}

impl<T> Nodes<T> {
    fn iter(nodes: Option<&Nodes<T>>) -> impl Iterator<Item = &T> {
        nodes
            .into_iter()
            .flat_map(|nodes| nodes.nodes.iter().flatten())
    }
}

#[derive(Deserialize)]
struct RawAuthor {
    #[serde(rename = "__typename", default)]
    typename: String,
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReview {
    state: String,
    /// Unset while the review is still a draft.
    submitted_at: Option<String>,
    author: Option<RawAuthor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComment {
    created_at: String,
    author: Option<RawAuthor>,
}

#[derive(Deserialize)]
struct RawCommitNode {
    commit: RawCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCommit {
    /// A git timestamp, in the committer's own time zone.
    committed_date: String,
}

/// Empty for the other timeline items, which the query doesn't ask for.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawForcePush {
    created_at: Option<String>,
}

fn timestamp(text: &str) -> Option<jiff::Timestamp> {
    text.parse().ok()
}

impl RawPr {
    /// Whether `author` is someone giving feedback: not the pull request's
    /// author, not a bot such as CI, and not an account the config ignores.
    fn is_reviewer(&self, author: Option<&RawAuthor>, ignore: &[String]) -> bool {
        let Some(author) = author else { return false };
        author.typename != "Bot"
            && !ignore.contains(&author.login)
            && self
                .author
                .as_ref()
                .is_none_or(|own| own.login != author.login)
    }

    /// When someone else last reviewed or commented.
    fn last_feedback(&self, ignore: &[String]) -> Option<jiff::Timestamp> {
        // A dismissed review has been withdrawn, often by a push that made
        // an approval stale.
        let reviews = Nodes::iter(self.reviews.as_ref())
            .filter(|review| review.state != "DISMISSED")
            .filter(|review| self.is_reviewer(review.author.as_ref(), ignore))
            .filter_map(|review| review.submitted_at.as_deref());
        let comments = Nodes::iter(self.comments.as_ref())
            .filter(|comment| self.is_reviewer(comment.author.as_ref(), ignore))
            .map(|comment| comment.created_at.as_str());
        reviews.chain(comments).filter_map(timestamp).max()
    }

    /// When the branch last changed: its newest commit, or a force push
    /// after it.
    fn last_push(&self) -> Option<jiff::Timestamp> {
        let commits =
            Nodes::iter(self.commits.as_ref()).map(|node| node.commit.committed_date.as_str());
        let force_pushes =
            Nodes::iter(self.timeline_items.as_ref()).filter_map(|push| push.created_at.as_deref());
        commits.chain(force_pushes).filter_map(timestamp).max()
    }
}

impl RawPr {
    fn into_pr(self, ignore_authors: &[String]) -> Pr {
        let raw = self;
        let last_feedback = raw.last_feedback(ignore_authors);
        let status = match (raw.state.as_str(), raw.review_decision.as_deref()) {
            ("MERGED", _) => Status::Merged,
            ("CLOSED", _) => Status::Closed,
            (_, Some("APPROVED")) => Status::Approved,
            (_, Some("CHANGES_REQUESTED")) => Status::ChangesRequested,
            // reviewDecision has no verdict for feedback that only
            // commented, so look for the feedback itself.
            _ if last_feedback.is_some() => Status::Commented,
            _ => Status::Pending,
        };
        let needs_rereview = matches!(status, Status::Commented | Status::ChangesRequested)
            && match (last_feedback, raw.last_push()) {
                (Some(feedback), Some(push)) => push > feedback,
                _ => false,
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
            needs_rereview,
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

    fn pull_request(node: Option<Value>) -> Option<Pr> {
        super::pull_request(node, &[])
    }

    fn node(state: &str, decision: Option<&str>) -> Value {
        json!({
            "__typename": "PullRequest",
            "id": "PR_1", "number": 1, "title": "t", "url": "u",
            "state": state, "isDraft": false,
            "baseRefName": "main", "headRefName": "h", "updatedAt": "2026-01-01T00:00:00Z",
            "repository": { "nameWithOwner": "o/r" },
            "reviewDecision": decision,
            "stackEntry": { "position": 2, "stack": { "number": 7 } },
            "author": { "login": "me" },
            "reviews": { "nodes": [] },
            "comments": { "nodes": [] },
            "commits": { "nodes": [{ "commit": { "committedDate": "2026-01-01T10:00:00Z" } }] },
            "timelineItems": { "nodes": [] },
        })
    }

    fn review(node: &mut Value, login: &str, kind: &str, at: &str) {
        node["reviews"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "state": "COMMENTED", "submittedAt": at, "author": { "__typename": kind, "login": login },
            }));
    }

    fn comment(node: &mut Value, login: &str, kind: &str, at: &str) {
        node["comments"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "createdAt": at, "author": { "__typename": kind, "login": login },
            }));
    }

    fn pushed(node: &mut Value, at: &str) {
        node["commits"]["nodes"][0]["commit"]["committedDate"] = json!(at);
    }

    #[test]
    fn a_merged_pull_request_is_merged_whatever_its_reviews_said() {
        let pr = pull_request(Some(node("MERGED", Some("CHANGES_REQUESTED")))).unwrap();
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
    fn only_someone_elses_comment_counts_as_commented() {
        let mut open = node("OPEN", Some("REVIEW_REQUIRED"));
        review(&mut open, "me", "User", "2026-01-01T11:00:00Z");
        comment(&mut open, "ci", "Bot", "2026-01-01T11:00:00Z");
        review(&mut open, "them", "User", "2026-01-01T11:00:00Z");
        open["reviews"]["nodes"][1]["state"] = json!("DISMISSED");
        assert_eq!(
            pull_request(Some(open.clone())).unwrap().status,
            Status::Pending
        );

        comment(&mut open, "them", "User", "2026-01-01T11:00:00Z");
        let pr = pull_request(Some(open)).unwrap();
        assert_eq!(pr.status, Status::Commented);
        assert_eq!(pr.marks(), vec![Status::Commented]);
    }

    #[test]
    fn ignored_authors_are_not_feedback() {
        let mut open = node("OPEN", None);
        comment(&mut open, "automation", "User", "2026-01-01T11:00:00Z");
        let pr = super::pull_request(Some(open), &["automation".to_string()]).unwrap();
        assert_eq!(pr.status, Status::Pending);
    }

    #[test]
    fn a_push_after_comments_needs_rereview_instead() {
        let mut open = node("OPEN", None);
        review(&mut open, "them", "User", "2026-01-01T11:00:00Z");
        // 12:30 in UTC: later than the review, though its clock reads 07:30.
        pushed(&mut open, "2026-01-01T07:30:00-05:00");
        let pr = pull_request(Some(open)).unwrap();
        assert!(pr.needs_rereview);
        assert_eq!(pr.marks(), vec![Status::NeedsRereview]);
    }

    #[test]
    fn a_force_push_counts_as_a_push() {
        let mut open = node("OPEN", None);
        comment(&mut open, "them", "User", "2026-01-01T11:00:00Z");
        open["timelineItems"]["nodes"] = json!([{ "createdAt": "2026-01-01T12:00:00Z" }]);
        assert!(pull_request(Some(open)).unwrap().needs_rereview);
    }

    #[test]
    fn changes_requested_then_pushed_shows_both() {
        let mut open = node("OPEN", Some("CHANGES_REQUESTED"));
        review(&mut open, "them", "User", "2026-01-01T09:00:00Z");
        let pr = pull_request(Some(open.clone())).unwrap();
        assert_eq!(
            pr.marks(),
            vec![Status::ChangesRequested, Status::NeedsRereview]
        );

        // Their comment after the push hands it back to the author.
        comment(&mut open, "them", "User", "2026-01-01T12:00:00Z");
        let pr = pull_request(Some(open)).unwrap();
        assert_eq!(pr.marks(), vec![Status::ChangesRequested]);
    }

    #[test]
    fn an_approval_is_not_reopened_by_a_push() {
        let mut open = node("OPEN", Some("APPROVED"));
        review(&mut open, "them", "User", "2026-01-01T09:00:00Z");
        assert_eq!(
            pull_request(Some(open)).unwrap().marks(),
            vec![Status::Approved]
        );
    }

    #[test]
    fn issues_are_passed_over() {
        assert_eq!(pull_request(Some(json!({ "__typename": "Issue" }))), None);
    }
}
