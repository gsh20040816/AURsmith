//! `aursmith-builder run`：家用 Builder 守护进程；`aursmith-builder guest`：构建容器入口。

mod client;
mod config;
mod guest;
mod runner;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "aursmith-builder", version, about = "AURsmith 家用 Builder")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 连接 aursmithd，循环领取并执行构建。
    Run,
    /// 在构建容器内执行 makepkg（由 Builder 通过 docker run 调用，不要手动运行）。
    Guest,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Guest => {
            guest::main();
            Ok(())
        }
        Command::Run => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();
            let config = config::BuilderConfig::from_env()?;
            tokio::runtime::Runtime::new()?.block_on(runner::run(config))
        }
    }
}
