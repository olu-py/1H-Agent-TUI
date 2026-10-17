use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use protium_tui::{app, config::Config};

#[derive(Debug, Parser)]
#[command(name = "1h-agent-tui", version, about)]
struct Cli {
    /// Directory the agent is allowed to access.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    /// Optional TOML configuration path.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Target display refresh rate in millihertz for stream redraws (120 Hz by default).
    /// Pass the verified desktop rate during physical presentation measurements.
    #[arg(long, default_value_t = 120_000, value_parser = clap::value_parser!(u32).range(1..=1_000_000))]
    refresh_rate_millihz: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let workspace = cli
        .workspace
        .canonicalize()
        .with_context(|| format!("cannot open workspace {}", cli.workspace.display()))?;
    let config = Config::load(cli.config.as_deref(), &workspace)?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "protium_tui=info,protium_core=info".into()),
        )
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

    app::run(workspace, config, cli.refresh_rate_millihz).await
}
