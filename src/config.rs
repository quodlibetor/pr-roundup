//! The TOML configuration file.

use std::path::{Path, PathBuf};

use color_eyre::{
    Result, Section, SectionExt,
    eyre::{WrapErr, eyre},
};
use serde::Deserialize;

use crate::model::Status;

pub const EXAMPLE: &str = r#"# Repositories whose open pull requests by you are listed.
repos = []
# Organizations: every repository in them is searched.
orgs = []

# port = 7845
# GitHub is polled when the page is opened or focused, at most this often.
# "Refresh now" ignores the limit.
# min_refresh_secs = 60

# What each status is written as in the text copied for Slack. Slack turns
# :name: into its emoji when the message is sent, if you want to use custom
# emoji.
[emoji]
# merged = "🟣"
# approved = "✅"
# changes_requested = "🔴"
# pending = "⏳"
# commented = "💬"
# closed = "⚫️"
"#;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_min_refresh")]
    pub min_refresh_secs: u64,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub orgs: Vec<String>,
    #[serde(default)]
    pub emoji: Emoji,
}

fn default_port() -> u16 {
    7845
}

fn default_min_refresh() -> u64 {
    60
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Emoji {
    pub pending: String,
    pub commented: String,
    pub approved: String,
    pub changes_requested: String,
    pub merged: String,
    pub closed: String,
}

impl Default for Emoji {
    fn default() -> Self {
        Emoji {
            pending: "⏳".into(),
            commented: "💬".into(),
            approved: ":github-approved:".into(),
            changes_requested: ":github-changes-requested:".into(),
            merged: ":git-merged:".into(),
            closed: "🚫".into(),
        }
    }
}

impl Emoji {
    pub fn for_status(&self, status: Status) -> &str {
        match status {
            Status::Pending => &self.pending,
            Status::Commented => &self.commented,
            Status::Approved => &self.approved,
            Status::ChangesRequested => &self.changes_requested,
            Status::Merged => &self.merged,
            Status::Closed => &self.closed,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("reading {}", path.display()))
            .with_section(|| EXAMPLE.to_string().header("An example config:"))?;
        let config: Config =
            toml::from_str(&text).wrap_err_with(|| format!("parsing {}", path.display()))?;
        if config.repos.is_empty() && config.orgs.is_empty() {
            return Err(eyre!("{} lists no repos or orgs", path.display()))
                .with_section(|| EXAMPLE.to_string().header("An example config:"));
        }
        Ok(config)
    }

    /// One GitHub search per configured repository or organization.
    pub fn searches(&self) -> Vec<String> {
        let scopes = self
            .repos
            .iter()
            .map(|repo| format!("repo:{repo}"))
            .chain(self.orgs.iter().map(|org| format!("org:{org}")));
        scopes
            .map(|scope| format!("{scope} is:pr is:open author:@me archived:false"))
            .collect()
    }
}

fn xdg_dir(var: &str, fallback: &str) -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(var).filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| eyre!("neither ${var} nor $HOME is set"))?;
    Ok(PathBuf::from(home).join(fallback))
}

pub fn default_config_path() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?
        .join("pr-link-copier")
        .join("config.toml"))
}

pub fn default_db_path() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_DATA_HOME", ".local/share")?
        .join("pr-link-copier")
        .join("tags.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_parses_and_uses_the_default_emoji() {
        let config: Config = toml::from_str(EXAMPLE).unwrap();
        assert_eq!(config.emoji.merged, ":git-merged:");
        assert_eq!(config.searches().len(), 1);
    }

    #[test]
    fn an_emoji_can_be_overridden_alone() {
        let config: Config =
            toml::from_str("repos = [\"o/r\"]\n[emoji]\npending = \":eyes:\"\n").unwrap();
        assert_eq!(config.emoji.pending, ":eyes:");
        assert_eq!(config.emoji.approved, ":github-approved:");
    }
}
