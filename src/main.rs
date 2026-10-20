use agently_cli_mcp::cli::CliArgs;
use agently_cli_mcp::cli_proc::runner::Runner;
use agently_cli_mcp::config::Transport;
use agently_cli_mcp::{config, error, logging, mcp};
use clap::Parser;

#[tokio::main]
async fn main() -> error::Result<()> {
    let cli = CliArgs::parse();

    let print_config = cli.print_config;
    let do_check = cli.check;

    let cfg = config::load(cli)?;

    logging::init(&cfg.logging)?;

    if do_check {
        check(&cfg).await?;
    }

    if print_config {
        let json = serde_json::to_string_pretty(&cfg.redacted())
            .map_err(|e| error::AppError::Other(e.to_string()))?;
        println!("{json}");
    }

    // `--check` and `--print-config` are one-shot diagnostics that exit after
    // running; only a bare invocation (neither flag) falls through to serving.
    if do_check || print_config {
        return Ok(());
    }

    match cfg.transport {
        Transport::Stdio => mcp::stdio::serve(cfg).await,
        Transport::Http => {
            let http_cfg = cfg.http.clone();
            let state = mcp::build_state(cfg)?;
            mcp::http::serve(state, &http_cfg, tokio_util::sync::CancellationToken::new()).await
        }
    }
}

/// Run the `--check` diagnostics: locate the CLI, print its version and auth
/// status, and fail (nonzero exit) if either command does not succeed.
async fn check(cfg: &config::Config) -> error::Result<()> {
    use std::time::Duration;

    let runner = Runner::new(&cfg.cli)?;
    let timeout = Duration::from_secs(cfg.cli.timeout_secs);

    let version = runner.run(&["--version".to_string()], timeout).await?;
    if version.exit_code != Some(0) {
        return Err(error::AppError::Other(format!(
            "`agently-cli --version` failed: {}",
            version.stderr.trim()
        )));
    }
    println!("{}", String::from_utf8_lossy(&version.stdout).trim());

    let auth = runner
        .run(&["auth".to_string(), "status".to_string()], timeout)
        .await?;
    if auth.exit_code != Some(0) {
        return Err(error::AppError::Other(format!(
            "`agently-cli auth status` failed: {}",
            auth.stderr.trim()
        )));
    }
    println!("{}", String::from_utf8_lossy(&auth.stdout).trim());

    Ok(())
}
