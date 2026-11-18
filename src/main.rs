//! A local web page listing your open pull requests, grouped by tags you
//! give them, ready to copy into Slack.

mod app;
mod board;
mod cli;
mod config;
mod github;
mod model;
mod store;
mod web;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::{CommandFactory, Parser, Subcommand};
use color_eyre::{Result, eyre::WrapErr};
use tracing::{info, warn};

#[derive(Parser)]
#[command(about)]
struct Args {
    /// Defaults to $XDG_CONFIG_HOME/pr-roundup/config.toml.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Where tags are kept. Defaults to $XDG_DATA_HOME/pr-roundup/tags.db.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
    /// With no subcommand, `serve` runs with these.
    #[command(flatten)]
    serve: Serve,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the page (the default). Runs `init` instead when there is no
    /// config file.
    Serve(Serve),
    /// Write an example config file to edit.
    Init,
    /// List, set or remove pull requests' tags.
    #[command(subcommand)]
    Tags(Tags),
    /// Install the skill that teaches Claude Code agents to tag pull
    /// requests, under ~/.claude/skills.
    InstallSkill {
        /// Replace an installed copy that differs from this version's.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum Tags {
    /// List every tag and its pull requests.
    List,
    /// Give a pull request a tag, replacing any it had.
    Set {
        /// A GitHub URL or owner/repo#123.
        pr: cli::PrRef,
        tag: String,
    },
    /// Remove a pull request's tag.
    Unset {
        /// A GitHub URL or owner/repo#123.
        pr: cli::PrRef,
    },
}

#[derive(clap::Args)]
struct Serve {
    /// Overrides the config file's port.
    #[arg(long)]
    port: Option<u16>,
    /// Open the page in a browser once the server is listening.
    #[arg(long)]
    open: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pr_roundup=info".into()),
        )
        .init();

    let args = Args::parse();
    // `serve`'s options also work without the subcommand, but mean nothing
    // to the others. clap's own setting for this would also turn away
    // --config and --db written before a subcommand.
    if args.command.is_some() && (args.serve.port.is_some() || args.serve.open) {
        Args::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "--port and --open belong to `serve`; put them after it, or leave out the subcommand",
            )
            .exit();
    }
    let config_path = match args.config {
        Some(path) => path,
        None => config::default_config_path()?,
    };
    let db_path = match args.db {
        Some(path) => path,
        None => config::default_db_path()?,
    };
    match args.command {
        Some(Command::Init) => init(&config_path),
        Some(Command::Serve(serve_args)) => serve(&config_path, &db_path, serve_args).await,
        None => serve(&config_path, &db_path, args.serve).await,
        Some(Command::Tags(Tags::List)) => cli::tags(&store::Store::open(&db_path)?),
        Some(Command::Tags(Tags::Set { pr, tag })) => {
            let config = config::Config::load(&config_path)?;
            cli::tag(&config, &store::Store::open(&db_path)?, &pr, &tag).await
        }
        Some(Command::Tags(Tags::Unset { pr })) => {
            let config = config::Config::load(&config_path)?;
            cli::untag(&config, &store::Store::open(&db_path)?, &pr).await
        }
        Some(Command::InstallSkill { force }) => cli::install_skill(force),
    }
}

fn init(config_path: &Path) -> Result<()> {
    config::Config::init(config_path)?;
    println!(
        "Wrote {}. Add the repositories or organizations to search to `repos` or `orgs` in it, then run pr-roundup again.",
        config_path.display()
    );
    Ok(())
}

async fn serve(config_path: &Path, db_path: &Path, args: Serve) -> Result<()> {
    if !config_path.exists() {
        return init(config_path);
    }
    let config = config::Config::load(config_path)?;
    let port = args.port.unwrap_or(config.port);
    let store = store::Store::open(db_path)?;
    let github = github::GitHub::new(config.ignore_authors.clone()).await?;
    let app = Arc::new(app::App::new(config, github, store)?);

    tokio::spawn(Arc::clone(&app).poll_on_request());
    // Fetch now, so the first page load finds current data instead of
    // showing stale tags and then swapping in the result of its own check.
    app.request_refresh(true)?;

    // Only this machine: the clipboard API needs a secure context, which
    // localhost is, and nothing here is meant for anyone else.
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .wrap_err_with(|| format!("binding {addr}"))?;
    let url = format!("http://{addr}");
    info!("serving on {url}");
    if args.open {
        // Opening can block until the browser starts, and failing to is no
        // reason to stop serving.
        tokio::task::spawn_blocking(move || {
            if let Err(error) = webbrowser::open(&url) {
                warn!(%error, "could not open a browser at {url}");
            }
        });
    }
    axum::serve(listener, web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
