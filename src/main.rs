use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::config::Transport;
use agently_cli_mcp::{config, error, logging, mcp};
use clap::Parser;

#[tokio::main]
async fn main() -> error::Result<()> {
    let cli = CliArgs::parse();

    let print_config = cli.print_config.unwrap_or(false);

    let cfg = config::load(cli)?;

    logging::init(&cfg.logging)?;

    if print_config {
        let json = serde_json::to_string_pretty(&cfg.redacted())
            .map_err(|e| error::AppError::Other(e.to_string()))?;
        println!("{json}");
        return Ok(());
    }

    match cfg.transport {
        Transport::Stdio => mcp::stdio::serve(cfg).await,
        Transport::Http => Err(error::AppError::Other(
            "HTTP transport is not implemented yet".to_string(),
        )),
    }
}
