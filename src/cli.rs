use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "agentic-compact", version, about)]
pub struct Cli {
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Json)]
    pub log_format: LogFormat,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the MCP stdio server and same-thread transition orchestrator.
    Mcp,
    /// Launch stock Codex through the shared LocalDaemon topology.
    Codex(CodexArgs),
    /// Validate the installation and optionally run disposable contract probes.
    Doctor(DoctorArgs),
    /// Install the binary integration, MCP config section and plugin package.
    Install(InstallArgs),
    /// Remove only files and configuration owned by agentic-compact.
    Uninstall(UninstallArgs),
    /// Print the agentic-compact version.
    Version,
}

#[derive(Debug, Args)]
pub struct CodexArgs {
    #[arg(last = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    /// Run billable, thread-mutating probes on a disposable ephemeral thread.
    #[arg(long)]
    pub probe: bool,

    /// Confirm that the stock-TUI reentrant attach gate was run for this Codex build.
    #[arg(long)]
    pub ack_reentrant_attach: bool,

    /// Confirm that injected checkpoint items stay out of the visible stock-TUI transcript.
    #[arg(long)]
    pub ack_hidden_checkpoint: bool,

    /// Include non-secret paths and raw check details in the report.
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Debug, Clone, Args)]
pub struct InstallArgs {
    /// Override the target CODEX_HOME.
    #[arg(long, value_name = "DIR")]
    pub codex_home: Option<PathBuf>,

    /// Do not modify shell startup files; print the wrapper command instead.
    #[arg(long)]
    pub no_shell_alias: bool,
}

#[derive(Debug, Clone, Args)]
pub struct UninstallArgs {
    /// Override the target CODEX_HOME.
    #[arg(long, value_name = "DIR")]
    pub codex_home: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LogFormat {
    Text,
    Json,
}
