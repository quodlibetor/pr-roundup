//! Tags and repository order, kept in SQLite.
//!
//! A tagged pull request keeps a copy of the last thing GitHub said about it,
//! so it can still be shown, merged or not, before the first poll finishes.

use std::{collections::HashMap, path::Path};

use color_eyre::{Result, eyre::WrapErr};
use rusqlite::{Connection, params};

use crate::model::Pr;

/// The key the untagged section's repository order is kept under.
pub const UNTAGGED: &str = "";

pub struct Store {
    conn: Connection,
}

pub struct Tagged {
    pub tag: String,
    pub snapshot: Pr,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).wrap_err_with(|| format!("creating {}", dir.display()))?;
        }
        let conn =
            Connection::open(path).wrap_err_with(|| format!("opening {}", path.display()))?;
        Store::init(conn)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tags (
                 pr_id TEXT PRIMARY KEY,
                 tag TEXT NOT NULL,
                 snapshot TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS repo_order (
                 tag TEXT NOT NULL,
                 repo TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 PRIMARY KEY (tag, repo)
             );",
        )?;
        Ok(Store { conn })
    }

    pub fn tagged(&self) -> Result<HashMap<String, Tagged>> {
        let mut statement = self.conn.prepare("SELECT pr_id, tag, snapshot FROM tags")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut tagged = HashMap::new();
        for row in rows {
            let (id, tag, snapshot) = row?;
            let snapshot: Pr = serde_json::from_str(&snapshot)
                .wrap_err_with(|| format!("reading the saved copy of {id}"))?;
            tagged.insert(id, Tagged { tag, snapshot });
        }
        Ok(tagged)
    }

    pub fn set_tag(&self, pr: &Pr, tag: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tags (pr_id, tag, snapshot) VALUES (?1, ?2, ?3)
             ON CONFLICT (pr_id) DO UPDATE SET tag = excluded.tag, snapshot = excluded.snapshot",
            params![pr.id, tag, serde_json::to_string(pr)?],
        )?;
        Ok(())
    }

    pub fn clear_tag(&self, pr_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM tags WHERE pr_id = ?1", params![pr_id])?;
        Ok(())
    }

    /// Refresh the saved copy of every tagged pull request in `prs`.
    pub fn update_snapshots(&self, prs: &[Pr]) -> Result<()> {
        let mut statement = self
            .conn
            .prepare("UPDATE tags SET snapshot = ?2 WHERE pr_id = ?1")?;
        for pr in prs {
            statement.execute(params![pr.id, serde_json::to_string(pr)?])?;
        }
        Ok(())
    }

    /// Each section's repositories in the order they were dragged into.
    pub fn repo_orders(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut statement = self
            .conn
            .prepare("SELECT tag, repo FROM repo_order ORDER BY tag, position")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut orders: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let (tag, repo) = row?;
            orders.entry(tag).or_default().push(repo);
        }
        Ok(orders)
    }

    pub fn set_repo_order(&mut self, tag: &str, repos: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM repo_order WHERE tag = ?1", params![tag])?;
        for (position, repo) in repos.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO repo_order (tag, repo, position) VALUES (?1, ?2, ?3)",
                params![tag, repo, i64::try_from(position)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;

    fn pr(id: &str, status: Status) -> Pr {
        Pr {
            id: id.into(),
            repo: "o/r".into(),
            number: 1,
            title: "t".into(),
            url: "u".into(),
            status,
            draft: false,
            base: "main".into(),
            head: "h".into(),
            updated_at: String::new(),
            stack: None,
        }
    }

    #[test]
    fn a_tag_keeps_the_latest_copy_of_its_pull_request() {
        let store = Store::in_memory().unwrap();
        store.set_tag(&pr("a", Status::Pending), "x").unwrap();
        store
            .update_snapshots(&[pr("a", Status::Merged), pr("untagged", Status::Pending)])
            .unwrap();
        let tagged = store.tagged().unwrap();
        assert_eq!(tagged.len(), 1);
        assert_eq!(tagged["a"].tag, "x");
        assert_eq!(tagged["a"].snapshot.status, Status::Merged);

        store.clear_tag("a").unwrap();
        assert!(store.tagged().unwrap().is_empty());
    }

    #[test]
    fn a_new_repo_order_replaces_the_old_one() {
        let mut store = Store::in_memory().unwrap();
        store
            .set_repo_order("x", &["a".into(), "b".into()])
            .unwrap();
        store.set_repo_order("x", &["b".into()]).unwrap();
        assert_eq!(store.repo_orders().unwrap()["x"], vec!["b".to_string()]);
    }
}
