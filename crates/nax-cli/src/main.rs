use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use nax_core::{recovery, Archive, CreateOptions, Error, ExtractOptions, Overwrite, Phase, Preset, Progress};

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
        /// Encrypt with a password (asked for; or set NAX_PASSWORD). Hides file names too.
        #[arg(short = 'P', long)]
        password: bool,
        /// Add a recovery record, in percent (e.g. 5): repairs damage up to that size
        #[arg(short, long, value_name = "PERCENT")]
        recovery: Option<f64>,
        /// Split into parts of this size, e.g. 100M, 700M, 4G
        #[arg(short, long, value_name = "SIZE", value_parser = parse_size)]
        volume: Option<u64>,
    },
    /// Repair a damaged archive using its recovery record (writes a new copy)
    Repair { archive: PathBuf },
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

/// "100M", "4G", "700m", "1.5g", "65536" -> bytes.
fn parse_size(s: &str) -> std::result::Result<u64, String> {
    let s = s.trim().to_ascii_lowercase();
    let (num, mult) = match s.trim_end_matches('b').chars().last() {
        Some('k') => (&s[..s.trim_end_matches('b').len() - 1], 1u64 << 10),
        Some('m') => (&s[..s.trim_end_matches('b').len() - 1], 1 << 20),
        Some('g') => (&s[..s.trim_end_matches('b').len() - 1], 1 << 30),
        _ => (s.trim_end_matches('b'), 1),
    };
    let v: f64 = num.trim().parse().map_err(|_| format!("not a size: {s} (try 100M or 4G)"))?;
    let bytes = (v * mult as f64) as u64;
    if bytes < nax_core::volume::MIN_VOLUME {
        return Err("parts must be at least 64K".into());
    }
    Ok(bytes)
}

/// Password from NAX_PASSWORD or typed (hidden). `confirm` asks twice.
fn ask_password(confirm: bool) -> Result<String> {
    if let Ok(p) = std::env::var("NAX_PASSWORD") {
        return Ok(p);
    }
    loop {
        let p = rpassword::prompt_password("Password: ")?;
        if p.is_empty() {
            eprintln!("The password can't be empty.");
            continue;
        }
        if confirm && rpassword::prompt_password("Repeat password: ")? != p {
            eprintln!("The passwords don't match, try again.");
            continue;
        }
        return Ok(p);
    }
}

/// Opens an archive, asking for the password if it is encrypted.
fn open(path: &PathBuf) -> Result<Archive> {
    match Archive::open(path) {
        Err(Error::PasswordRequired) => {}
        other => return Ok(other?),
    }
    let from_env = std::env::var("NAX_PASSWORD").is_ok();
    for _ in 0..3 {
        match Archive::open_with(path, Some(&ask_password(false)?)) {
            Err(Error::WrongPassword) if !from_env => eprintln!("Wrong password."),
            other => return Ok(other?),
        }
    }
    Err(Error::WrongPassword.into())
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
        Phase::Protecting => "Protecting",
        Phase::Repairing => "Repairing",
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
        Command::Add { mut archive, inputs, preset, threads, overwrite, block_mb, password, recovery, volume } => {
            if archive.extension().is_none() {
                archive.set_extension(nax_core::EXTENSION);
            }
            let password = if password { Some(ask_password(true)?) } else { None };
            let opts = CreateOptions {
                preset,
                threads,
                overwrite,
                block_size: block_mb.map(|mb| mb << 20),
                cancel: Some(cancel),
                password,
                recovery_percent: recovery.unwrap_or(0.0),
                volume_size: volume,
                ..Default::default()
            };
            let pb = progress_bar();
            let stats = nax_core::create(&archive, &inputs, &opts, &mut |p| update(&pb, p));
            pb.finish_and_clear();
            let s = stats?;
            for w in &s.warnings {
                eprintln!("warning: {w}");
            }
            let name = match s.outputs.as_slice() {
                [one] => one.display().to_string(),
                parts => format!("{} … ({} parts)", parts[0].display(), parts.len()),
            };
            println!(
                "Created {} ({} preset)\n  {} files, {} folders · {} → {} ({}) · {:.1} s · {} · {} threads",
                name,
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
            if opts.password.is_some() {
                println!("  Encrypted with AES-256 (file names hidden). Without the password nobody can open it, including you.");
            }
            if let Some(r) = s.recovery {
                println!("  Recovery record {:.1}%: can repair up to {} of damage", r.percent(), human(r.capacity_bytes()));
            }
        }
        Command::Repair { archive } => {
            let pb = progress_bar();
            let r = recovery::repair(&archive, Some(&cancel), &mut |p| update(&pb, p));
            pb.finish_and_clear();
            let r = r?;
            match (&r.info, r.damaged_shards, r.repairable) {
                (None, _, _) => anyhow::bail!("this archive has no recovery record, so it can't be repaired"),
                (Some(_), 0, _) => println!(
                    "No damage found: the archive is intact{}.",
                    if r.damaged_parity > 0 { " (part of the recovery record itself is damaged)" } else { "" }
                ),
                (Some(_), _, true) => println!(
                    "Repaired {} of damage. Repaired copy: {}",
                    human(r.damaged_bytes),
                    r.output.first().map(|p| p.display().to_string()).unwrap_or_default()
                ),
                (Some(i), _, false) => anyhow::bail!(
                    "too much damage to repair: about {} damaged, the recovery record covers {}",
                    human(r.damaged_bytes),
                    human(i.capacity_bytes())
                ),
            }
        }
        Command::Extract { archive, files, out, overwrite, threads } => {
            let a = open(&archive)?;
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
            let a = open(&archive)?;
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
            let a = open(&archive)?;
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
