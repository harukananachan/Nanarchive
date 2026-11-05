use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use nax_core::{Archive, CreateOptions, ExtractOptions, Overwrite, Phase, Preset, Progress};

#[derive(Parser)]
#[command(name = "nax", version, about = "Nanarchive — compress smaller, beautifully")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an archive from files and folders
    #[command(alias = "a")]
    Add {
        /// Archive to create (.nax is added if no extension is given)
        archive: PathBuf,
        /// Files and folders to add
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// store, fast, balanced, max or ultra
        #[arg(short, long, default_value = "balanced")]
        preset: Preset,
        /// Number of threads (default: automatic)
        #[arg(short, long)]
        threads: Option<usize>,
        /// Replace the archive if it exists
        #[arg(short = 'y', long)]
        overwrite: bool,
        /// Block size in MB (advanced; default comes from the preset)
        #[arg(long, hide = true)]
        block_mb: Option<usize>,
    },
    /// Extract an archive
    #[command(alias = "x")]
    Extract {
        archive: PathBuf,
        /// Only extract these files or folders
        files: Vec<String>,
        /// Output folder
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        /// Replace existing files (default: keep them)
        #[arg(short = 'y', long)]
        overwrite: bool,
        #[arg(short, long)]
        threads: Option<usize>,
    },
    /// List archive contents
    #[command(alias = "l")]
    List { archive: PathBuf },
    /// Check that an archive is intact
    #[command(alias = "t")]
    Test {
        archive: PathBuf,
        #[arg(short, long)]
        threads: Option<usize>,
    },
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        "–".into()
    } else {
        format!("{:.1}%", part as f64 * 100.0 / whole as f64)
    }
}

fn speed(bytes: u64, t: Duration) -> String {
    let s = t.as_secs_f64().max(0.001);
    format!("{}/s", human((bytes as f64 / s) as u64))
}

fn progress_bar() -> ProgressBar {
    let pb = ProgressBar::new(0);
    pb.set_style(
        ProgressStyle::with_template(
            "{msg:<12} [{bar:32.magenta/blue}] {percent:>3}%  {binary_bytes}/{binary_total_bytes}  {binary_bytes_per_sec}  eta {eta}",
        )
        .unwrap()
        .progress_chars("█▓░"),
    );
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}

fn update(pb: &ProgressBar, p: &Progress) {
    let msg = match p.phase {
        Phase::Scanning => "Scanning",
        Phase::Compressing => "Compressing",
        Phase::Finishing => "Finishing",
        Phase::Extracting => "Extracting",
        Phase::Testing => "Testing",
    };
    pb.set_message(msg);
    pb.set_length(p.bytes_total);
    pb.set_position(p.bytes_done);
}

fn cancel_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = flag.clone();
    let _ = ctrlc::set_handler(move || {
        if f.swap(true, Ordering::Relaxed) {
            std::process::exit(130); // second Ctrl+C: leave now
        }
        eprintln!("\nStopping… (press Ctrl+C again to force)");
    });
    flag
}

fn run(cli: Cli) -> Result<()> {
    let cancel = cancel_flag();
    match cli.command {
        Command::Add { mut archive, inputs, preset, threads, overwrite, block_mb } => {
            if archive.extension().is_none() {
                archive.set_extension(nax_core::EXTENSION);
            }
            let opts = CreateOptions {
                preset,
                threads,
                overwrite,
                block_size: block_mb.map(|mb| mb << 20),
                cancel: Some(cancel),
            };
            let pb = progress_bar();
            let stats = nax_core::create(&archive, &inputs, &opts, &mut |p| update(&pb, p));
            pb.finish_and_clear();
            let s = stats?;
            for w in &s.warnings {
                eprintln!("warning: {w}");
            }
            println!(
                "Created {} ({} preset)\n  {} files, {} folders · {} → {} ({}) · {:.1} s · {} · {} threads",
                archive.display(),
                preset,
                s.files,
                s.dirs,
                human(s.bytes_in),
                human(s.archive_size),
                percent(s.archive_size, s.bytes_in),
                s.elapsed.as_secs_f64(),
                speed(s.bytes_in, s.elapsed),
                s.threads,
            );
        }
        Command::Extract { archive, files, out, overwrite, threads } => {
            let a = Archive::open(&archive)?;
            let opts = ExtractOptions {
                overwrite: if overwrite { Overwrite::Replace } else { Overwrite::Skip },
                threads,
                only: files,
                cancel: Some(cancel),
            };
            let pb = progress_bar();
            let stats = a.extract(&out, &opts, &mut |p| update(&pb, p));
            pb.finish_and_clear();
            let s = stats?;
            println!(
                "Extracted {} files, {} folders ({}) to {} · {:.1} s · {}",
                s.files,
                s.dirs,
                human(s.bytes),
                out.display(),
                s.elapsed.as_secs_f64(),
                speed(s.bytes, s.elapsed),
            );
            if s.skipped > 0 {
                println!("  {} existing files kept (use -y to replace them)", s.skipped);
            }
        }
        Command::List { archive } => {
            let a = Archive::open(&archive)?;
            println!("{:>10}  {:<16}  Name", "Size", "Modified");
            println!("{:>10}  {:<16}  ----", "----", "--------");
            let (mut files, mut dirs) = (0u64, 0u64);
            for e in a.entries() {
                let size = if e.is_dir() {
                    dirs += 1;
                    "<folder>".to_string()
                } else {
                    files += 1;
                    human(e.size)
                };
                let when = e
                    .mtime_ns
                    .and_then(|ns| {
                        chrono::DateTime::from_timestamp(
                            ns.div_euclid(1_000_000_000),
                            ns.rem_euclid(1_000_000_000) as u32,
                        )
                    })
                    .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                println!("{size:>10}  {when:<16}  {}", e.path);
            }
            println!(
                "\n{files} files, {dirs} folders · {} → {} ({}) · {} · {} blocks",
                human(a.total_size()),
                human(a.archive_size()),
                percent(a.archive_size(), a.total_size()),
                a.methods().join(", "),
                a.blocks().len(),
            );
        }
        Command::Test { archive, threads } => {
            let a = Archive::open(&archive)?;
            let pb = progress_bar();
            let stats = a.test(threads, Some(&cancel), &mut |p| update(&pb, p));
            pb.finish_and_clear();
            let s = stats?;
            println!(
                "OK: {} files, {} checked in {:.1} s ({}) — archive is intact",
                s.files,
                human(s.bytes),
                s.elapsed.as_secs_f64(),
                speed(s.bytes, s.elapsed),
            );
        }
    }
    Ok(())
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
