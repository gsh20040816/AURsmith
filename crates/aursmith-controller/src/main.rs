mod audits;
mod auth;
mod backups;
mod config;
mod db;
mod error;
mod notifications;
mod packages;
mod profiles;
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
    RestoreControlPlane {
        #[arg(long)]
        backup: std::path::PathBuf,
    },
    TransferSourceId,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "aursmith=info".into()))
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    let cli = Cli::parse();
    let config = Config::from_env()?;
    if let Some(Command::RestoreControlPlane { backup }) = &cli.command {
        backups::restore(&config, &backup).await?;
        println!("控制面数据库已从签名备份恢复；原数据库保留在同目录的 recovery 子目录中");
        return Ok(());
    }
    if matches!(&cli.command, Some(Command::TransferSourceId)) {
        let database = db::connect(&config.database_url).await?;
        let state = routes::AppState::new(database, config.clone(), config.load_signing_key()?);
        println!("{}", backups::transfer_source_id(&state));
        return Ok(());
    }

    let database = db::connect(&config.database_url).await?;
    let signing_key = config.load_signing_key()?;
    config.materialize_ssh_identity()?;
    let state = routes::AppState::new(database, config.clone(), signing_key);
    backups::spawn_export_socket(state.clone()).await?;
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
