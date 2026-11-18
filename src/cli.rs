//! Tagging from the command line, for people and agents. These work on the
//! database directly, so the server needn't be running; when it is, it's told
//! to update its open pages.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    io::Write as _,
    path::PathBuf,
    str::FromStr,
    time::Duration,
};

use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};

use crate::{board, config::Config, github::GitHub, model::Pr, store::Store};

/// The agent skill that `install-skill` writes.
pub const SKILL: &str = include_str!("../.claude/skills/pr-roundup/SKILL.md");

/// A pull request named on the command line: a GitHub URL or `owner/repo#123`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrRef {
    pub repo: String,
    pub number: u64,
}

impl FromStr for PrRef {
    type Err = String;

    fn from_str(text: &str) -> Result<PrRef, String> {
        let invalid = || format!("`{text}` isn't a pull request URL or owner/repo#123");
        let text = text.trim();
        let (repo, number) = if let Some(path) = text
            .strip_prefix("https://github.com/")
            .or_else(|| text.strip_prefix("http://github.com/"))
        {
            // owner/repo/pull/123, perhaps followed by /files, #discussion…
            let mut parts = path.split(['/', '#', '?']);
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some(owner), Some(name), Some("pull"), Some(number)) => {
                    (format!("{owner}/{name}"), number)
                }
                _ => return Err(invalid()),
            }
        } else {
            let (repo, number) = text.split_once('#').ok_or_else(invalid)?;
            (repo.to_string(), number)
        };
        let number = number.parse().map_err(|_| invalid())?;
        match repo.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Ok(PrRef { repo, number })
            }
            _ => Err(invalid()),
        }
    }
}

impl std::fmt::Display for PrRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

pub async fn tag(config: &Config, store: &Store, pr: &PrRef, tag: &str) -> Result<()> {
    let tag = tag.trim();
    if tag.is_empty() {
        return Err(eyre!("the tag is empty; use `tags unset` to remove one"));
    }
    let github = GitHub::new(config.ignore_authors.clone()).await?;
    let found = github.by_number(&pr.repo, pr.number).await?;
    // The rest of its stack: the repository's open pull requests, as the
    // page would list them, and any tagged ones.
    let tagged = store.tagged()?;
    let search = format!("repo:{} is:pr is:open author:@me archived:false", pr.repo);
    let mut known = github.pull_requests(&[search], &[]).await?;
    let tags: HashMap<String, String> = tagged
        .into_iter()
        .map(|(id, tagged)| {
            if !known.iter().any(|pr| pr.id == id) {
                known.push(tagged.snapshot);
            }
            (id, tagged.tag)
        })
        .collect();
    for member in board::stack_to_tag(&found, known, &tags) {
        store.set_tag(&member, tag)?;
        println!(
            "Tagged {}#{} \"{}\" as \"{tag}\"",
            member.repo, member.number, member.title
        );
    }
    notify_server(config.port).await;
    Ok(())
}

pub async fn untag(config: &Config, store: &Store, pr: &PrRef) -> Result<()> {
    let Some((id, tagged)) = store
        .tagged()?
        .into_iter()
        .find(|(_, tagged)| same(&tagged.snapshot, pr))
    else {
        println!("{pr} isn't tagged");
        return Ok(());
    };
    store.clear_tag(&id)?;
    println!("Removed the tag \"{}\" from {pr}", tagged.tag);
    notify_server(config.port).await;
    Ok(())
}

/// Every tag and its pull requests, as they were at the last poll.
pub fn tags(store: &Store) -> Result<()> {
    let mut by_tag: BTreeMap<String, Vec<Pr>> = BTreeMap::new();
    for tagged in store.tagged()?.into_values() {
        by_tag.entry(tagged.tag).or_default().push(tagged.snapshot);
    }
    let mut out = String::new();
    if by_tag.is_empty() {
        out.push_str("No pull requests are tagged.\n");
    }
    for (tag, mut prs) in by_tag {
        prs.sort_by(|a, b| (&a.repo, a.number).cmp(&(&b.repo, b.number)));
        let _ = writeln!(out, "{tag}");
        for pr in prs {
            let _ = writeln!(
                out,
                "  {}#{}  {}  {}",
                pr.repo,
                pr.number,
                pr.status.label().to_lowercase(),
                pr.title
            );
        }
    }
    // Output piped into `head` is closed early, which isn't an error.
    match std::io::stdout().lock().write_all(out.as_bytes()) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(error.into()),
        _ => Ok(()),
    }
}

fn same(pr: &Pr, reference: &PrRef) -> bool {
    pr.number == reference.number && pr.repo.eq_ignore_ascii_case(&reference.repo)
}

/// Asks a server on this machine to update its open pages. One that isn't
/// running is fine: it reads the tags when it starts.
async fn notify_server(port: u16) {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return;
    };
    let _ = client
        .post(format!("http://127.0.0.1:{port}/changed"))
        .send()
        .await;
}

/// Writes the agent skill under Claude Code's skills directory. An existing
/// copy that differs is only replaced with `force`, in case it was edited.
pub fn install_skill(force: bool) -> Result<()> {
    let base = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").ok_or_else(|| eyre!("$HOME isn't set"))?)
            .join(".claude"),
    };
    let path = base.join("skills").join("pr-roundup").join("SKILL.md");
    match std::fs::read_to_string(&path) {
        Ok(existing) if existing == SKILL => {
            println!("{} is already up to date", path.display());
            return Ok(());
        }
        Ok(_) if !force => {
            return Err(eyre!(
                "{} exists and differs from this version's skill; pass --force to replace it",
                path.display()
            ));
        }
        _ => {}
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).wrap_err_with(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&path, SKILL).wrap_err_with(|| format!("writing {}", path.display()))?;
    println!("Wrote {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<PrRef, String> {
        text.parse()
    }

    #[test]
    fn urls_and_short_forms_name_the_same_pull_request() {
        let expected = PrRef {
            repo: "lacework/services".into(),
            number: 36832,
        };
        for text in [
            "lacework/services#36832",
            "https://github.com/lacework/services/pull/36832",
            "https://github.com/lacework/services/pull/36832/files",
            "https://github.com/lacework/services/pull/36832#discussion_r1",
            " lacework/services#36832\n",
        ] {
            assert_eq!(parse(text), Ok(expected.clone()), "{text}");
        }
    }

    #[test]
    fn other_text_is_rejected() {
        for text in [
            "36832",
            "services#36832",
            "lacework/services#abc",
            "https://github.com/lacework/services/issues/36832",
            "https://github.com/lacework/services",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn the_skill_has_frontmatter_claude_code_reads() {
        assert!(SKILL.starts_with("---\nname: pr-roundup\ndescription: "));
    }
}
