mod auth;
mod config;
mod db;
mod error;
mod routes;
mod scheduler;
mod transport;

use anyhow::Context;
use clap::{Parser, Subcommand};
use config::Config;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(name = "aursmith-controller", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve,
    SetupToken,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "aursmith=info".into()))
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    let cli = Cli::parse();
    let config = Config::from_env()?;
    if matches!(cli.command, Some(Command::SetupToken)) {
        println!("{}", config.setup_token);
        return Ok(());
    }

    let database = db::connect(&config.database_url).await?;
    let signing_key = config.load_signing_key()?;
    let state = routes::AppState::new(database, config.clone(), signing_key);
    scheduler::spawn(state.clone());
    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .with_context(|| format!("无法监听 {}", config.bind_address))?;
    tracing::info!(address = %config.bind_address, "Controller 已启动");
    axum::serve(listener, app)
        .await
        .context("Controller 服务异常退出")
}
