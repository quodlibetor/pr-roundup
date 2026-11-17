//! Shared state and the loop that keeps it current.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use color_eyre::{Result, eyre::eyre};
use tokio::sync::{Notify, broadcast};
use tracing::{info, warn};

use crate::{
    board::{self, Board},
    config::Config,
    github::GitHub,
    model::Pr,
    store::Store,
};

pub struct App {
    pub config: Config,
    pub github: GitHub,
    pub store: Mutex<Store>,
    pub latest: RwLock<Latest>,
    /// Told whenever what the page shows may have changed.
    pub updates: broadcast::Sender<()>,
    /// Wakes the poller early.
    pub refresh: Notify,
}

/// The outcome of the last poll.
#[derive(Default)]
pub struct Latest {
    pub prs: Vec<Pr>,
    /// Seconds since the epoch; `None` until a poll has succeeded.
    pub fetched_at: Option<u64>,
    pub error: Option<String>,
}

impl App {
    pub fn new(config: Config, github: GitHub, store: Store) -> Result<App> {
        // Show the tagged pull requests as they last were until the first
        // poll comes back.
        let prs = store
            .tagged()?
            .into_values()
            .map(|tagged| tagged.snapshot)
            .collect();
        Ok(App {
            config,
            github,
            store: Mutex::new(store),
            latest: RwLock::new(Latest {
                prs,
                ..Latest::default()
            }),
            updates: broadcast::channel(16).0,
            refresh: Notify::new(),
        })
    }

    pub fn store(&self) -> Result<std::sync::MutexGuard<'_, Store>> {
        self.store
            .lock()
            .map_err(|_| eyre!("the tag store's lock is poisoned"))
    }

    pub fn changed(&self) {
        // Nobody listening is fine.
        let _ = self.updates.send(());
    }

    /// The pull request with this node id, from the last poll or a tag.
    pub fn find(&self, id: &str) -> Result<Option<Pr>> {
        let found = self
            .latest
            .read()
            .map_err(|_| eyre!("lock poisoned"))?
            .prs
            .iter()
            .find(|pr| pr.id == id)
            .cloned();
        match found {
            Some(pr) => Ok(Some(pr)),
            None => Ok(self
                .store()?
                .tagged()?
                .remove(id)
                .map(|tagged| tagged.snapshot)),
        }
    }

    pub fn board(&self) -> Result<(Board, Vec<String>)> {
        let (tagged, orders) = {
            let store = self.store()?;
            (store.tagged()?, store.repo_orders()?)
        };
        let mut prs = self
            .latest
            .read()
            .map_err(|_| eyre!("lock poisoned"))?
            .prs
            .clone();
        let present: HashSet<String> = prs.iter().map(|pr| pr.id.clone()).collect();
        let mut tags = HashMap::new();
        for (id, tagged) in tagged {
            if !present.contains(&id) {
                prs.push(tagged.snapshot);
            }
            tags.insert(id, tagged.tag);
        }
        let mut names: Vec<String> = tags.values().cloned().collect();
        names.sort();
        names.dedup();
        Ok((board::build(prs, &tags, &orders), names))
    }

    async fn poll_once(&self) -> Result<()> {
        let mut prs = self.github.search(&self.config.searches()).await?;
        let tagged: Vec<String> = self.store()?.tagged()?.into_keys().collect();
        let missing: Vec<String> = tagged
            .into_iter()
            .filter(|id| !prs.iter().any(|pr| pr.id == *id))
            .collect();
        if !missing.is_empty() {
            prs.extend(self.github.by_id(&missing).await?);
        }
        self.store()?.update_snapshots(&prs)?;
        let mut latest = self.latest.write().map_err(|_| eyre!("lock poisoned"))?;
        latest.prs = prs;
        latest.fetched_at = Some(now());
        latest.error = None;
        Ok(())
    }

    pub async fn poll_forever(self: Arc<Self>) {
        let interval = Duration::from_secs(self.config.poll_interval_secs.max(10));
        loop {
            match self.poll_once().await {
                Ok(()) => info!("polled GitHub"),
                Err(error) => {
                    warn!("polling GitHub failed: {error:?}");
                    if let Ok(mut latest) = self.latest.write() {
                        latest.error = Some(format!("{error:#}"));
                    }
                }
            }
            self.changed();
            tokio::select! {
                () = tokio::time::sleep(interval) => {}
                () = self.refresh.notified() => {}
            }
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
