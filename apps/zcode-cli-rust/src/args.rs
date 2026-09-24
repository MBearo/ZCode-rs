use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version, about = "ZCode CLI Rust runtime")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Serve the App stdio protocol (NDJSON on stdout, diagnostics on stderr).
    AppServer(AppServerArgs),
    /// Interactive terminal UI. Reserved entry point; not implemented yet.
    Tui,
}

#[derive(clap::Args, Debug)]
pub struct AppServerArgs {
    #[arg(long, required = true)]
    pub stdio: bool,
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long, value_parser=["desktop","terminal"], default_value="terminal")]
    pub surface: String,
    #[arg(long)]
    pub prepare_storage: bool,
}
