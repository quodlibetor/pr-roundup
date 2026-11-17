//! A local web page listing your open pull requests, grouped by tags you
//! give them, ready to copy into Slack.

mod app;
mod board;
mod config;
mod github;
mod model;
mod store;
mod web;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use clap::Parser;
use color_eyre::{Result, eyre::WrapErr};
use tracing::info;

#[derive(Parser)]
#[command(about)]
struct Args {
    /// Defaults to $XDG_CONFIG_HOME/pr-roundup/config.toml.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Where tags are kept. Defaults to $XDG_DATA_HOME/pr-roundup/tags.db.
    #[arg(long)]
    db: Option<PathBuf>,
    /// Overrides the config file's port.
    #[arg(long)]
    port: Option<u16>,
    /// Print an example config file and exit.
    #[arg(long)]
    example_config: bool,
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
    if args.example_config {
        print!("{}", config::EXAMPLE);
        return Ok(());
    }

    let config_path = match args.config {
        Some(path) => path,
        None => config::default_config_path()?,
    };
    let db_path = match args.db {
        Some(path) => path,
        None => config::default_db_path()?,
    };
    let config = config::Config::load(&config_path)?;
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
