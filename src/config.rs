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
pending = "🟠"
changes_requested = "🔴"
commented = "💬"
approved = "🟢"
merged = "🟣"
closed = "⚫️"
# pending = "🟠"
# changes_requested = ":github-changes-requested:"
# commented = "💬"
# approved = ":github-approved:"
# merged = ":github-merged:"
# closed = ":github-closed:"
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
    pub emoji: Emoji,
}

fn default_port() -> u16 {
    7845
}

fn default_min_refresh() -> u64 {
    60
}

/// Every status must be given: `init` writes them all.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Emoji {
    pub pending: String,
    pub commented: String,
    pub approved: String,
    pub changes_requested: String,
    pub merged: String,
    pub closed: String,
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
            return Err(eyre!(
                "{} lists no repos or orgs: add the repositories (\"owner/repo\") \
                 or organizations to search to `repos` or `orgs` in it",
                path.display()
            ));
        }
        Ok(config)
    }

    /// Writes the example config to `path`, refusing to replace a file
    /// already there.
    pub fn init(path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).wrap_err_with(|| format!("creating {}", dir.display()))?;
        }
        let mut file = std::fs::File::create_new(path).wrap_err_with(|| {
            if path.exists() {
                format!("{} already exists", path.display())
            } else {
                format!("creating {}", path.display())
            }
        })?;
        std::io::Write::write_all(&mut file, EXAMPLE.as_bytes())
            .wrap_err_with(|| format!("writing {}", path.display()))
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
        .join("pr-roundup")
        .join("config.toml"))
}

pub fn default_db_path() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_DATA_HOME", ".local/share")?
        .join("pr-roundup")
        .join("tags.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_parses_and_sets_every_emoji() {
        let config: Config = toml::from_str(EXAMPLE).unwrap();
        assert_eq!(config.emoji.merged, "🟣");
    }

    #[test]
    fn init_writes_a_config_that_load_rejects_until_repos_are_added() {
        let dir = std::env::temp_dir().join(format!("pr-roundup-test-{}", std::process::id()));
        let path = dir.join("nested").join("config.toml");
        Config::init(&path).unwrap();
        let error = Config::load(&path).unwrap_err().to_string();
        assert!(error.contains("lists no repos or orgs"), "{error}");
        let again = Config::init(&path).unwrap_err().to_string();
        assert!(again.contains("already exists"), "{again}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_emoji_is_an_error() {
        let error = toml::from_str::<Config>("repos = [\"o/r\"]\n[emoji]\npending = \":eyes:\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing field"), "{error}");
    }
}
