use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::{config, error, logging};
use clap::Parser;

fn main() -> error::Result<()> {
    let cli = CliArgs::parse();

    let print_config = cli.print_config.unwrap_or(false);

    let cfg = config::load(cli)?;

    logging::init(&cfg.logging)?;

    if print_config {
        let json = serde_json::to_string_pretty(&cfg.redacted())
            .map_err(|e| error::AppError::Other(e.to_string()))?;
        println!("{json}");
    }

    Ok(())
}
