mod admin;
mod aur;
mod auth;
mod config;
mod credentials;
mod db;
mod error;
mod packages;
mod reviews;
mod web;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(name = "aursmith", version, about = "单管理员 AUR 私有仓库核心")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 启动服务端 HTML 和包目录管理服务。
    Serve(ServeArgs),
    /// 只在公网核心设备本地管理唯一管理员。
    Admin(AdminArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, env = "AURSMITH_DATABASE_PATH", value_name = "PATH")]
    database_path: PathBuf,
    #[arg(long, env = "AURSMITH_BIND", default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    #[arg(long, env = "AURSMITH_PUBLIC_ORIGIN")]
    public_origin: String,
    #[arg(long, env = "AURSMITH_SESSION_IDLE_MINUTES", default_value_t = 60)]
    session_idle_minutes: i64,
    #[arg(long, env = "AURSMITH_SESSION_ABSOLUTE_HOURS", default_value_t = 168)]
    session_absolute_hours: i64,
}

#[derive(Debug, Args)]
struct AdminArgs {
    #[arg(long, env = "AURSMITH_DATABASE_PATH", value_name = "PATH")]
    database_path: PathBuf,
    #[command(subcommand)]
    command: AdminCommand,
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    Init {
        #[arg(long, default_value = "admin")]
        username: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    ResetPassword {
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    RevokeSessions,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve(arguments) => serve(arguments).await,
        Command::Admin(arguments) => run_admin(arguments).await,
    }
}

async fn serve(arguments: ServeArgs) -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "aursmith=info".into()))
        .with(tracing_subscriber::fmt::layer().json())
        .init();
    let config = config::Config::new(
        arguments.bind,
        arguments.database_path,
        &arguments.public_origin,
        arguments.session_idle_minutes,
        arguments.session_absolute_hours,
    )?;
    let database = db::open_or_create(&config.database_path).await?;
    let app = web::router(web::AppState::new(database, config.clone()));
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("无法监听 {}", config.bind))?;
    tracing::info!(address = %config.bind, "AURsmith 已启动");
    axum::serve(listener, app)
        .await
        .context("AURsmith 服务异常退出")
}

async fn run_admin(arguments: AdminArgs) -> anyhow::Result<()> {
    let database = db::open_existing(&arguments.database_path, 1).await?;
    let result = match arguments.command {
        AdminCommand::Init {
            username,
            password_file,
        } => {
            let password = admin::read_password(password_file.as_deref())?;
            admin::initialize(&database, &username, &password).await?
        }
        AdminCommand::ResetPassword { password_file } => {
            let password = admin::read_password(password_file.as_deref())?;
            admin::reset_password(&database, &password).await?
        }
        AdminCommand::RevokeSessions => admin::revoke_sessions(&database).await?,
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
