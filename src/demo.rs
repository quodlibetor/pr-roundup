//! The page over invented pull requests, for the README's screenshots
//! (`mise run screenshots`). `pr-roundup demo` is hidden from `--help`.
//!
//! It polls nothing and needs no token or config, and its store is in
//! memory, so nothing leaves the machine and nothing is left behind. Tags
//! set on the page last until it stops. The pull requests are in [`prs`];
//! edit them to change what the screenshots show.

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use color_eyre::{Result, eyre::WrapErr};
use tokio::signal::unix::{SignalKind, signal};

use crate::{
    app::App,
    config::{self, Config},
    github::GitHub,
    model::{Pr, StackEntry, Status},
    store::Store,
    web,
};

/// Seeds the page and serves it on `port` (0 picks a free one) until
/// interrupted. The screenshot script reads the URL from the first line
/// printed.
pub async fn serve(port: u16) -> Result<()> {
    let app = Arc::new(app()?);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .wrap_err_with(|| format!("binding {addr}"))?;
    println!("http://{}", listener.local_addr()?);
    let mut term = signal(SignalKind::terminate()).wrap_err("listening for SIGTERM")?;
    axum::serve(listener, web::router(app))
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        })
        .await?;
    Ok(())
}

/// The app over the seeded store, as if a poll had just come back. Nothing
/// polls, so "Refresh now" does nothing.
fn app() -> Result<App> {
    let mut config: Config =
        toml::from_str(config::EXAMPLE).wrap_err("parsing the example config")?;
    config.repos = vec!["quodlibetor/frobnicator".into()];
    let store = Store::in_memory()?;
    let prs = prs();
    for (pr, tag) in &prs {
        if let Some(tag) = tag {
            store.set_tag(pr, tag)?;
        }
    }
    // Never asked anything: nothing polls.
    let github = GitHub::with_token("demo".into(), Vec::new())?;
    let app = App::new(config, github, store)?;
    {
        let mut latest = app
            .latest
            .write()
            .map_err(|_| color_eyre::eyre::eyre!("lock poisoned"))?;
        latest.prs = prs.into_iter().map(|(pr, _)| pr).collect();
        // A minute ago, so the header reads "Updated 1 minute ago".
        latest.fetched_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs())
                .saturating_sub(60),
        );
    }
    Ok(app)
}

/// Every pull request the page shows, with its tag. You are `quodlibetor`;
/// the repositories are made up.
fn prs() -> Vec<(Pr, Option<&'static str>)> {
    let counting = Some("counting");
    let wheel = Some("hamster wheel");
    vec![
        // A GitHub stack, bottom first: merged, approved, then pushed to
        // since review.
        (
            pr(
                "quodlibetor/frobnicator",
                41,
                "Count with a u64 instead of fingers",
            )
            .status(Status::Merged)
            .stacked(1, 1),
            counting,
        ),
        (
            pr(
                "quodlibetor/frobnicator",
                42,
                "Teach the frobnicator to count past three",
            )
            .status(Status::Approved)
            .stacked(1, 2),
            counting,
        ),
        (
            pr(
                "quodlibetor/frobnicator",
                43,
                "Say \"lots\" instead of overflowing",
            )
            .status(Status::ChangesRequested)
            .rereview()
            .stacked(1, 3),
            counting,
        ),
        (
            pr(
                "quodlibetor/frobnicator-docs",
                7,
                "Document counting past three",
            )
            .status(Status::Pending),
            counting,
        ),
        // Chained by branch: #19's base is #18's head.
        (
            pr(
                "quodlibetor/hamster-wheel",
                18,
                "Replace the hamster wheel with a slightly faster hamster wheel",
            )
            .status(Status::Commented)
            .branches("main", "faster-wheel"),
            wheel,
        ),
        (
            pr("quodlibetor/hamster-wheel", 19, "Oil the new wheel")
                .status(Status::Pending)
                .branches("faster-wheel", "oil")
                .draft(),
            wheel,
        ),
        (
            pr(
                "quodlibetor/flock",
                23,
                "Make flock wait politely instead of tapping its foot",
            )
            .status(Status::Approved),
            wheel,
        ),
        // Untagged.
        (
            pr("quodlibetor/frobnicator", 44, "Bump the frobnicator's MSRV")
                .status(Status::Pending),
            None,
        ),
        (
            pr(
                "quodlibetor/flock",
                25,
                "Spell \"lock\" consistently in the docs",
            )
            .status(Status::Commented),
            None,
        ),
    ]
}

/// An open pull request awaiting review, on its own branch off `main`.
fn pr(repo: &str, number: u64, title: &str) -> Pr {
    Pr {
        id: format!("PR_{repo}#{number}"),
        repo: repo.into(),
        number,
        title: title.into(),
        url: format!("https://github.com/{repo}/pull/{number}"),
        status: Status::Pending,
        draft: false,
        base: "main".into(),
        head: format!("pr-{number}"),
        updated_at: format!("2026-06-12T15:{number:02}:00Z"),
        stack: None,
        needs_rereview: false,
    }
}

trait Build {
    fn status(self, status: Status) -> Self;
    fn rereview(self) -> Self;
    fn draft(self) -> Self;
    fn stacked(self, stack: u64, position: u64) -> Self;
    fn branches(self, base: &str, head: &str) -> Self;
}

impl Build for Pr {
    fn status(self, status: Status) -> Pr {
        Pr { status, ..self }
    }

    fn rereview(self) -> Pr {
        Pr {
            needs_rereview: true,
            ..self
        }
    }

    fn draft(self) -> Pr {
        Pr {
            draft: true,
            ..self
        }
    }

    fn stacked(self, stack: u64, position: u64) -> Pr {
        Pr {
            stack: Some(StackEntry { stack, position }),
            ..self
        }
    }

    fn branches(self, base: &str, head: &str) -> Pr {
        Pr {
            base: base.into(),
            head: head.into(),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_seeded_pull_request_is_on_the_board() {
        let app = app().unwrap();
        let board = app.board().unwrap();
        let shown: Vec<u64> = board
            .sections
            .iter()
            .flat_map(|section| &section.repos)
            .flat_map(|group| group.stacks.iter().flatten())
            .map(|pr| pr.number)
            .collect();
        for (pr, _) in prs() {
            assert!(shown.contains(&pr.number), "#{} isn't shown", pr.number);
        }
        let tags: Vec<Option<&str>> = board.sections.iter().map(|s| s.tag.as_deref()).collect();
        assert_eq!(tags, [Some("counting"), Some("hamster wheel"), None]);
    }
}
