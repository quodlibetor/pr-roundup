//! A local web page listing your open pull requests, grouped by tags you
//! give them, ready to copy into Slack.

mod app;
mod board;
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

use clap::{Parser, Subcommand};
use color_eyre::{Result, eyre::WrapErr};
use tracing::info;

#[derive(Parser)]
#[command(about, args_conflicts_with_subcommands = true)]
struct Args {
    /// Defaults to $XDG_CONFIG_HOME/pr-roundup/config.toml.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
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
}

#[derive(clap::Args)]
struct Serve {
    /// Where tags are kept. Defaults to $XDG_DATA_HOME/pr-roundup/tags.db.
    #[arg(long)]
    db: Option<PathBuf>,
    /// Overrides the config file's port.
    #[arg(long)]
    port: Option<u16>,
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
    let config_path = match args.config {
        Some(path) => path,
        None => config::default_config_path()?,
    };
    match args.command {
        Some(Command::Init) => init(&config_path),
        Some(Command::Serve(serve_args)) => serve(&config_path, serve_args).await,
        None => serve(&config_path, args.serve).await,
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

async fn serve(config_path: &Path, args: Serve) -> Result<()> {
    if !config_path.exists() {
        return init(config_path);
    }
    let db_path = match args.db {
        Some(path) => path,
        None => config::default_db_path()?,
    };
    let config = config::Config::load(config_path)?;
    let port = args.port.unwrap_or(config.port);
    let store = store::Store::open(&db_path)?;
    let github = github::GitHub::new().await?;
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
    info!("serving on http://{addr}");
    axum::serve(listener, web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
