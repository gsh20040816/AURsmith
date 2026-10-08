//! aursmithd：AURsmith 公网主服务与运维子命令。

mod admin;
mod app;
mod aur;
mod auth;
mod builds;
mod config;
mod db;
mod error;
mod packages;
mod publish;
mod review;
mod routes;
mod signer;
mod state;

#[cfg(test)]
mod tests;

use anyhow::Context;
use clap::{Parser, Subcommand};
use sqlx::Connection;
use std::{path::PathBuf, time::Duration};

#[derive(Debug, Parser)]
#[command(name = "aursmithd", version, about = "AURsmith 主服务")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 运行 Web/API 与后台循环（同步、审查、调和）。
    Serve,
    /// 管理唯一管理员。
    Admin {
        #[arg(
            long,
            env = "AURSMITH_DATABASE_URL",
            default_value = "sqlite:///var/lib/aursmith/aursmith.db"
        )]
        database_url: String,
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// 导出当前库为状态 JSON（迁移或升级前的安全备份）。
    Export {
        #[arg(
            long,
            env = "AURSMITH_DATABASE_URL",
            default_value = "sqlite:///var/lib/aursmith/aursmith.db"
        )]
        database_url: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// 只读打开旧版数据库并转换为状态 JSON。
    LegacyExport {
        #[arg(long)]
        legacy_database_url: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// 把状态 JSON 导入空的新库，并在提交前逐表校验。
    Import {
        #[arg(
            long,
            env = "AURSMITH_DATABASE_URL",
            default_value = "sqlite:///var/lib/aursmith/aursmith.db"
        )]
        database_url: String,
        #[arg(long)]
        input: PathBuf,
    },
    /// 校验数据库内容与状态 JSON 一致。
    Verify {
        #[arg(
            long,
            env = "AURSMITH_DATABASE_URL",
            default_value = "sqlite:///var/lib/aursmith/aursmith.db"
        )]
        database_url: String,
        #[arg(long)]
        input: PathBuf,
    },
    /// 无网络签名器：处理 exchange inbox，签名并原子切换仓库。
    Signer {
        #[arg(
            long,
            env = "AURSMITH_EXCHANGE_DIR",
            default_value = "/var/lib/aursmith-exchange"
        )]
        exchange_dir: PathBuf,
        #[arg(long, env = "AURSMITH_REPOSITORY_DIR", default_value = "/srv/repo")]
        repository_dir: PathBuf,
        #[arg(
            long,
            env = "AURSMITH_SIGNER_GPG_KEY_FILE",
            default_value = "/run/secrets/repository_gpg_key"
        )]
        gpg_key_file: PathBuf,
        #[arg(long, default_value = "/run/aursmith-gnupg")]
        gpg_home: PathBuf,
        /// 紧急回滚到 previous release 后退出。
        #[arg(long)]
        rollback: bool,
    },
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

fn print(value: &impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn connect_plain(database_url: &str) -> anyhow::Result<sqlx::SqliteConnection> {
    let pool = db::connect(database_url).await?;
    pool.close().await;
    Ok(sqlx::SqliteConnection::connect(database_url).await?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    match Cli::parse().command {
        Command::Serve => serve().await,
        Command::Admin {
            database_url,
            command,
        } => {
            let database = db::connect(&database_url).await?;
            let result = match command {
                AdminCommand::Init {
                    username,
                    password_file,
                } => {
                    admin::init(
                        &database,
                        &username,
                        &admin::read_password(password_file.as_deref())?,
                    )
                    .await?
                }
                AdminCommand::ResetPassword { password_file } => {
                    admin::reset_password(
                        &database,
                        &admin::read_password(password_file.as_deref())?,
                    )
                    .await?
                }
                AdminCommand::RevokeSessions => admin::revoke_sessions(&database).await?,
            };
            print(&result)
        }
        Command::Export {
            database_url,
            output,
        } => {
            let mut connection = connect_plain(&database_url).await?;
            let document = state::export(&mut connection).await?;
            write_document(&output, &document)?;
            print(
                &serde_json::json!({"output": output, "counts": document.counts(), "tables_sha256": document.tables_sha256()}),
            )
        }
        Command::LegacyExport {
            legacy_database_url,
            output,
        } => {
            let document = state::legacy_export(&legacy_database_url).await?;
            write_document(&output, &document)?;
            print(
                &serde_json::json!({"output": output, "counts": document.counts(), "tables_sha256": document.tables_sha256(), "warnings": document.warnings}),
            )
        }
        Command::Import {
            database_url,
            input,
        } => {
            let document = read_document(&input)?;
            let mut connection = connect_plain(&database_url).await?;
            let counts = state::import(&mut connection, &document).await?;
            print(
                &serde_json::json!({"imported": counts, "verified": true, "tables_sha256": document.tables_sha256()}),
            )
        }
        Command::Verify {
            database_url,
            input,
        } => {
            let document = read_document(&input)?;
            let mut connection = connect_plain(&database_url).await?;
            let counts = state::verify(&mut connection, &document).await?;
            print(&serde_json::json!({"verified": true, "counts": counts}))
        }
        Command::Signer {
            exchange_dir,
            repository_dir,
            gpg_key_file,
            gpg_home,
            rollback,
        } => {
            let signer =
                signer::Signer::initialize(exchange_dir, repository_dir, &gpg_key_file, gpg_home)?;
            if rollback {
                let release = signer.rollback()?;
                return print(&serde_json::json!({"rolled_back_to": release}));
            }
            signer::run(&signer, Duration::from_secs(5))
        }
    }
}

fn write_document(path: &std::path::Path, document: &state::StateDocument) -> anyhow::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("无法创建 {}（不会覆盖已有文件）", path.display()))?;
    file.write_all(&serde_json::to_vec_pretty(document)?)?;
    file.sync_all()?;
    Ok(())
}

fn read_document(path: &std::path::Path) -> anyhow::Result<state::StateDocument> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("无法读取 {}", path.display()))?,
    )
    .context("状态文件格式无效")
}

async fn serve() -> anyhow::Result<()> {
    let config = config::Config::from_env()?;
    let database = db::connect(&config.database_url).await?;
    let aur = aur::AurClient::new(&config.aur_base_url, config.arch_https_proxy.as_deref())?;
    let reviewer = match &config.review_config {
        Some(path) => Some(review::Reviewer::from_file(path)?),
        None => {
            tracing::warn!("未设置 AURSMITH_REVIEW_CONFIG：所有 revision 将进入人工审查");
            None
        }
    };
    for directory in [
        config.data_dir.join("artifacts"),
        config.exchange_dir.join("inbox"),
        config.exchange_dir.join("outbox"),
    ] {
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("无法创建 {}", directory.display()))?;
    }
    let bind = config.bind_address.clone();
    let state = app::AppState::new(database, config, aur, reviewer);
    tokio::spawn(packages::run_sync_loop(state.clone()));
    tokio::spawn(review::run_loop(state.clone()));
    tokio::spawn(publish::run_loop(state.clone()));
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "aursmithd 已启动");
    axum::serve(listener, routes::router(state)).await?;
    Ok(())
}
