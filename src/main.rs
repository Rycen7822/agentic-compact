use agentic_compact::cli::{Cli, Command};
use agentic_compact::error::Result;
use clap::Parser;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("agentic-compact: {error}");
        std::process::exit(error.exit_code());
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    agentic_compact::observability::init(cli.log_format)?;

    match cli.command {
        Command::Mcp => agentic_compact::mcp::serve().await,
        Command::Codex(args) => agentic_compact::launcher::run(args.args).await,
        Command::Doctor(args) => agentic_compact::doctor::run(args).await,
        Command::Install(args) => agentic_compact::install::install(args).await,
        Command::Uninstall(args) => agentic_compact::install::uninstall(args).await,
        Command::Version => {
            println!("agentic-compact {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}
