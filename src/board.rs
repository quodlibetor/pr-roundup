//! Arranging pull requests into tag sections, repositories and stacks, and
//! writing a section out for Slack.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Write as _,
};

use crate::{config::Emoji, model::Pr, store::UNTAGGED};

#[derive(Debug)]
pub struct Board {
    pub sections: Vec<Section>,
}

#[derive(Debug)]
pub struct Section {
    /// `None` for the pull requests nobody has tagged, which come last.
    pub tag: Option<String>,
    pub repos: Vec<RepoGroup>,
}

#[derive(Debug)]
pub struct RepoGroup {
    pub repo: String,
    /// Each stack top first, the way `jj log` reads.
    pub stacks: Vec<Vec<Pr>>,
}

impl Board {
    /// The tags offered when tagging: those with open work on the page, so
    /// a tag whose pull requests have all merged stops being suggested.
    pub fn live_tags(&self) -> Vec<&str> {
        self.sections
            .iter()
            .filter(|section| {
                section
                    .repos
                    .iter()
                    .flat_map(|group| group.stacks.iter().flatten())
                    .any(|pr| !pr.is_finished())
            })
            .filter_map(|section| section.tag.as_deref())
            .collect()
    }
}

impl Section {
    /// The key this section's repository order is stored under.
    pub fn order_key(&self) -> &str {
        self.tag.as_deref().unwrap_or(UNTAGGED)
    }
}

/// Lay out `prs`. A pull request in `tags` goes in that tag's section;
/// one that isn't tagged is shown only while it is open.
pub fn build(
    prs: Vec<Pr>,
    tags: &HashMap<String, String>,
    orders: &HashMap<String, Vec<String>>,
) -> Board {
    let mut by_tag: BTreeMap<Option<String>, Vec<Pr>> = BTreeMap::new();
    for pr in prs {
        let tag = tags.get(&pr.id).cloned();
        if tag.is_none() && pr.is_finished() {
            continue;
        }
        by_tag.entry(tag).or_default().push(pr);
    }

    // `None` sorts first in a BTreeMap; the untagged section belongs last.
    let untagged = by_tag.remove(&None);
    let sections = by_tag
        .into_iter()
        .chain(untagged.map(|prs| (None, prs)))
        .map(|(tag, prs)| {
            let order = orders
                .get(tag.as_deref().unwrap_or(UNTAGGED))
                .map_or(&[][..], Vec::as_slice);
            Section {
                repos: repo_groups(prs, order),
                tag,
            }
        })
        .collect();
    Board { sections }
}

/// Split `prs` by repository, in the dragged `order`, with repositories the
/// order doesn't mention after it alphabetically.
fn repo_groups(prs: Vec<Pr>, order: &[String]) -> Vec<RepoGroup> {
    let mut by_repo: BTreeMap<String, Vec<Pr>> = BTreeMap::new();
    for pr in prs {
        by_repo.entry(pr.repo.clone()).or_default().push(pr);
    }
    let mut groups: Vec<RepoGroup> = by_repo
        .into_iter()
        .map(|(repo, prs)| RepoGroup {
            repo,
            stacks: stacks(prs),
        })
        .collect();
    groups.sort_by_key(|group| {
        order
            .iter()
            .position(|repo| *repo == group.repo)
            .unwrap_or(usize::MAX)
    });
    groups
}

/// Group one repository's pull requests into stacks.
///
/// GitHub's own stacks are taken first. The rest are chained by branch: a
/// pull request whose base is another's head sits on top of it. Stacks with
/// open work come before finished ones, most recently updated first.
pub fn stacks(prs: Vec<Pr>) -> Vec<Vec<Pr>> {
    let (stacked, loose): (Vec<Pr>, Vec<Pr>) = prs.into_iter().partition(|pr| pr.stack.is_some());

    let mut github: BTreeMap<u64, Vec<Pr>> = BTreeMap::new();
    for pr in stacked {
        if let Some(entry) = pr.stack {
            github.entry(entry.stack).or_default().push(pr);
        }
    }
    let mut stacks: Vec<Vec<Pr>> = github
        .into_values()
        .map(|mut members| {
            members.sort_by_key(|pr| Reverse(pr.stack.map_or(0, |entry| entry.position)));
            members
        })
        .collect();
    stacks.extend(branch_chains(loose));

    stacks.sort_by_cached_key(|stack| {
        let finished = stack.iter().all(Pr::is_finished);
        let newest = stack
            .iter()
            .map(|pr| pr.updated_at.clone())
            .max()
            .unwrap_or_default();
        (finished, Reverse(newest))
    });
    stacks
}

fn branch_chains(prs: Vec<Pr>) -> Vec<Vec<Pr>> {
    let by_head: HashMap<&str, usize> = prs
        .iter()
        .enumerate()
        .map(|(index, pr)| (pr.head.as_str(), index))
        .collect();
    let parent = |index: usize| {
        by_head
            .get(prs[index].base.as_str())
            .copied()
            .filter(|&parent| parent != index)
    };
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); prs.len()];
    for index in 0..prs.len() {
        if let Some(parent) = parent(index) {
            children[parent].push(index);
        }
    }

    let mut seen = HashSet::new();
    let mut chains: Vec<Vec<usize>> = Vec::new();
    // Roots first, then anything a cycle of bases left unreached.
    let starts = (0..prs.len())
        .filter(|&index| parent(index).is_none())
        .chain(0..prs.len());
    for start in starts {
        if seen.contains(&start) {
            continue;
        }
        let mut chain = Vec::new();
        let mut todo = vec![start];
        while let Some(index) = todo.pop() {
            if seen.insert(index) {
                chain.push(index);
                todo.extend(children[index].iter().rev());
            }
        }
        // Walked from the base up; shown from the top down.
        chain.reverse();
        chains.push(chain);
    }

    let mut slots: Vec<Option<Pr>> = prs.into_iter().map(Some).collect();
    chains
        .into_iter()
        .map(|chain| {
            chain
                .into_iter()
                .filter_map(|index| slots[index].take())
                .collect()
        })
        .collect()
}

/// What the copy button puts on the clipboard for one section.
pub struct Copied {
    pub text: String,
    pub html: String,
}

/// Write `section` out the way `jj spr list --format slack` does, under the
/// tag as a bold heading. Each stack is its own list.
pub fn copy(section: &Section, emoji: &Emoji) -> Copied {
    let stacks: Vec<&Vec<Pr>> = section
        .repos
        .iter()
        .flat_map(|group| &group.stacks)
        .collect();

    let mut text = String::new();
    let mut html = String::new();
    if let Some(tag) = &section.tag {
        let _ = writeln!(text, "*{tag}*");
        let _ = write!(html, "<p><b>{}</b></p>", escape(tag));
    }

    let lists: Vec<String> = stacks
        .iter()
        .map(|stack| {
            stack
                .iter()
                .map(|pr| {
                    format!(
                        "- {} {}\n  {}",
                        emoji.for_status(pr.status),
                        pr.title,
                        pr.url
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    text.push_str(&lists.join("\n\n"));

    for stack in stacks {
        html.push_str("<ul>");
        for pr in stack {
            let _ = write!(
                html,
                "<li>{} <a href=\"{}\">{}</a></li>",
                escape(emoji.for_status(pr.status)),
                escape(&pr.url),
                escape(&pr.title),
            );
        }
        html.push_str("</ul>");
    }

    Copied { text, html }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{StackEntry, Status};

    fn pr(id: &str, repo: &str, base: &str, head: &str, updated: &str) -> Pr {
        Pr {
            id: id.into(),
            repo: repo.into(),
            number: 1,
            title: format!("title {id}"),
            url: format!("https://github.com/{repo}/pull/{id}"),
            status: Status::Pending,
            draft: false,
            base: base.into(),
            head: head.into(),
            updated_at: updated.into(),
            stack: None,
        }
    }

    fn ids(stacks: &[Vec<Pr>]) -> Vec<Vec<&str>> {
        stacks
            .iter()
            .map(|stack| stack.iter().map(|pr| pr.id.as_str()).collect())
            .collect()
    }

    #[test]
    fn branches_chain_into_stacks_read_top_first() {
        let stacks = stacks(vec![
            pr("bottom", "o/r", "main", "b1", "1"),
            pr("top", "o/r", "b2", "b3", "1"),
            pr("middle", "o/r", "b1", "b2", "1"),
            pr("alone", "o/r", "main", "x", "2"),
        ]);
        assert_eq!(
            ids(&stacks),
            vec![vec!["alone"], vec!["top", "middle", "bottom"]]
        );
    }

    #[test]
    fn github_stacks_are_ordered_by_position() {
        let mut low = pr("low", "o/r", "main", "a", "1");
        low.stack = Some(StackEntry {
            stack: 3,
            position: 1,
        });
        let mut high = pr("high", "o/r", "main", "b", "1");
        high.stack = Some(StackEntry {
            stack: 3,
            position: 2,
        });
        assert_eq!(ids(&stacks(vec![low, high])), vec![vec!["high", "low"]]);
    }

    #[test]
    fn a_cycle_of_bases_still_shows_every_pull_request() {
        let stacks = stacks(vec![
            pr("a", "o/r", "y", "x", "1"),
            pr("b", "o/r", "x", "y", "1"),
        ]);
        assert_eq!(stacks.iter().map(Vec::len).sum::<usize>(), 2);
    }

    #[test]
    fn finished_stacks_go_last() {
        let mut merged = pr("merged", "o/r", "main", "a", "9");
        merged.status = Status::Merged;
        let stacks = stacks(vec![merged, pr("open", "o/r", "main", "b", "1")]);
        assert_eq!(ids(&stacks), vec![vec!["open"], vec!["merged"]]);
    }

    #[test]
    fn untagged_finished_pull_requests_are_dropped_and_tagged_ones_kept() {
        let mut merged = pr("merged", "o/r", "main", "a", "1");
        merged.status = Status::Merged;
        let mut kept = pr("kept", "o/r", "main", "b", "1");
        kept.status = Status::Merged;
        let tags = HashMap::from([("kept".to_string(), "work".to_string())]);
        let board = build(
            vec![merged, kept, pr("open", "o/r", "main", "c", "1")],
            &tags,
            &HashMap::new(),
        );
        let sections: Vec<_> = board.sections.iter().map(|s| s.tag.clone()).collect();
        assert_eq!(sections, vec![Some("work".to_string()), None]);
        assert_eq!(ids(&board.sections[1].repos[0].stacks), vec![vec!["open"]]);
    }

    #[test]
    fn only_tags_with_open_work_are_suggested() {
        let mut merged = pr("merged", "o/r", "main", "a", "1");
        merged.status = Status::Merged;
        let tags = HashMap::from([
            ("merged".to_string(), "done".to_string()),
            ("open".to_string(), "active".to_string()),
        ]);
        let board = build(
            vec![merged, pr("open", "o/r", "main", "b", "1")],
            &tags,
            &HashMap::new(),
        );
        assert_eq!(board.live_tags(), vec!["active"]);
    }

    #[test]
    fn repositories_follow_the_dragged_order_then_the_alphabet() {
        let prs = vec![
            pr("1", "a/a", "main", "1", "1"),
            pr("2", "b/b", "main", "2", "1"),
            pr("3", "c/c", "main", "3", "1"),
        ];
        let orders = HashMap::from([(UNTAGGED.to_string(), vec!["c/c".to_string()])]);
        let board = build(prs, &HashMap::new(), &orders);
        let repos: Vec<_> = board.sections[0]
            .repos
            .iter()
            .map(|g| g.repo.as_str())
            .collect();
        assert_eq!(repos, vec!["c/c", "a/a", "b/b"]);
    }

    #[test]
    fn a_section_copies_with_its_tag_as_a_heading() {
        let mut approved = pr("2", "o/r", "b1", "b2", "1");
        approved.status = Status::Approved;
        let section = Section {
            tag: Some("<rollout>".into()),
            repos: vec![RepoGroup {
                repo: "o/r".into(),
                stacks: vec![vec![approved], vec![pr("1", "o/r", "main", "b1", "1")]],
            }],
        };
        let copied = copy(&section, &Emoji::default());
        assert_eq!(
            copied.text,
            "*<rollout>*\n\
             - :github-approved: title 2\n  https://github.com/o/r/pull/2\n\
             \n\
             - ⏳ title 1\n  https://github.com/o/r/pull/1"
        );
        assert_eq!(
            copied.html,
            "<p><b>&lt;rollout&gt;</b></p>\
             <ul><li>:github-approved: <a href=\"https://github.com/o/r/pull/2\">title 2</a></li></ul>\
             <ul><li>⏳ <a href=\"https://github.com/o/r/pull/1\">title 1</a></li></ul>"
        );
    }
}
