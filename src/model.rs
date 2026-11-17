//! The pull requests the page shows, as the rest of the program sees them.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pr {
    /// GitHub's GraphQL node id, which survives a repository rename and is
    /// what a tagged pull request is looked up by once it leaves the search.
    pub id: String,
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub status: Status,
    pub draft: bool,
    pub base: String,
    pub head: String,
    /// RFC 3339, as GitHub sends it, which sorts correctly as a string.
    pub updated_at: String,
    pub stack: Option<StackEntry>,
    /// Someone else commented or requested changes, and the author has
    /// pushed since, so it is the reviewers' turn. Absent from snapshots
    /// saved before this was tracked.
    #[serde(default)]
    pub needs_rereview: bool,
}

impl Pr {
    pub fn is_finished(&self) -> bool {
        matches!(self.status, Status::Merged | Status::Closed)
    }

    /// The statuses a pull request is shown with. Needing re-review takes
    /// the place of "commented", but is shown next to "changes requested",
    /// whose verdict stands until the reviewer reviews again.
    pub fn marks(&self) -> Vec<Status> {
        match (self.status, self.needs_rereview) {
            (Status::Commented, true) => vec![Status::NeedsRereview],
            (Status::ChangesRequested, true) => {
                vec![Status::ChangesRequested, Status::NeedsRereview]
            }
            (status, _) => vec![status],
        }
    }
}

/// Where a pull request sits in one of GitHub's own stacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackEntry {
    pub stack: u64,
    /// 1 is the pull request nearest the base branch.
    pub position: u64,
}

/// Where a pull request stands: finished, or with its reviewers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Pending,
    Commented,
    Approved,
    ChangesRequested,
    /// Only ever one of [`Pr::marks`]; GitHub has no such state.
    NeedsRereview,
    Merged,
    Closed,
}

impl Status {
    /// What the page shows. Slack's own emoji names mean nothing to a
    /// browser, so the page does not use the configured ones.
    pub fn icon(self) -> &'static str {
        match self {
            Status::Pending => "⏳",
            Status::Commented => "💬",
            Status::Approved => "✅",
            Status::ChangesRequested => "🔴",
            Status::NeedsRereview => "🔵",
            Status::Merged => "🟣",
            Status::Closed => "🚫",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Pending => "Awaiting review",
            Status::Commented => "Commented",
            Status::Approved => "Approved",
            Status::ChangesRequested => "Changes requested",
            Status::NeedsRereview => "Needs re-review",
            Status::Merged => "Merged",
            Status::Closed => "Closed",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Commented => "commented",
            Status::Approved => "approved",
            Status::ChangesRequested => "changes-requested",
            Status::NeedsRereview => "needs-rereview",
            Status::Merged => "merged",
            Status::Closed => "closed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_saved_before_rereview_was_tracked_still_loads() {
        let pr: Pr = serde_json::from_str(
            r#"{"id":"PR_1","repo":"o/r","number":1,"title":"t","url":"u",
                "status":"Commented","draft":false,"base":"main","head":"h",
                "updated_at":"2026-01-01T00:00:00Z","stack":null}"#,
        )
        .unwrap();
        assert!(!pr.needs_rereview);
    }
}
