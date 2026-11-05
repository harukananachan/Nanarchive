use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use nax_core::{Error, Preset};

#[derive(Parser)]
#[command(name = "nax", version, about = "Nanarchive — compress smaller, beautifully")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add files to a new archive
    #[command(alias = "a")]
    Add {
        /// Archive to create (.nax)
        archive: PathBuf,
        /// Files and folders to add
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// store, fast, balanced, max or ultra
        #[arg(short, long, default_value = "balanced")]
        preset: Preset,
    },
    /// Extract an archive
    #[command(alias = "x")]
    Extract {
        archive: PathBuf,
        /// Output folder
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
    },
    /// List archive contents
    #[command(alias = "l")]
    List { archive: PathBuf },
    /// Test archive integrity
    #[command(alias = "t")]
    Test { archive: PathBuf },
}

fn run(cli: Cli) -> nax_core::Result<()> {
    match cli.command {
        Command::Add { .. } => Err(Error::NotImplemented("add (Phase 1)")),
        Command::Extract { .. } => Err(Error::NotImplemented("extract (Phase 1)")),
        Command::List { .. } => Err(Error::NotImplemented("list (Phase 1)")),
        Command::Test { .. } => Err(Error::NotImplemented("test (Phase 1)")),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nax: {e}");
            ExitCode::FAILURE
        }
    }
}
