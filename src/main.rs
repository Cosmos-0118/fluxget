mod engine;
mod http;
mod picker;
mod resolver;
mod sites;
mod state;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::{ffi::OsString, path::PathBuf, process::Command};

#[derive(Parser)]
#[command(
    version,
    about = "Adaptive downloads with verified resume. A bare URL implies 'get'."
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Download a file, or choose a detected page download with arrow keys
    Get(Download),
    /// Resume using the checkpoint beside an output file
    Resume {
        output: PathBuf,
        #[command(flatten)]
        network: Network,
        #[command(flatten)]
        tuning: Tuning,
    },
    /// Probe actual range support, size, validators, and redirects
    Inspect {
        url: String,
        #[command(flatten)]
        network: Network,
    },
    /// List public HTML download candidates without selecting one
    Resolve {
        url: String,
        #[command(flatten)]
        network: Network,
    },
    /// Show availability and versions of optional open-source backends
    Doctor,
    /// Delegate torrents, Metalink, FTP, or advanced mirrors to installed aria2c
    #[command(trailing_var_arg = true)]
    Aria2 {
        #[arg(allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Delegate supported media websites to installed yt-dlp
    #[command(trailing_var_arg = true)]
    Media {
        #[arg(allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
}

#[derive(Args, Clone)]
pub struct Network {
    /// Additional request header. Not persisted in checkpoints
    #[arg(short = 'H', long = "header")]
    headers: Vec<String>,
    /// HTTP/SOCKS proxy; environment HTTP_PROXY/HTTPS_PROXY are also supported
    #[arg(long)]
    proxy: Option<String>,
    /// Per-request timeout, including body transfer
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=86400))]
    timeout: u64,
}

#[derive(Args, Clone)]
pub struct Tuning {
    /// Maximum simultaneous range requests (auto starts at two)
    #[arg(short = 'j', long, default_value_t = 8, value_parser = clap::value_parser!(u8).range(1..=32))]
    connections: u8,
    /// Fixed connection count; otherwise adapt based on measured throughput
    #[arg(long)]
    fixed: bool,
    /// Force one continuous response, without range checkpoints
    #[arg(long)]
    single_stream: bool,
    /// Chunk size in KiB; buffers at most connections * chunk size
    #[arg(long, default_value_t = 2048, value_parser = clap::value_parser!(u32).range(64..=16384))]
    chunk_kib: u32,
    /// Retries per request after the initial attempt
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(0..=10))]
    retries: u8,
    /// Write one machine-readable result to stdout; suppress progress
    #[arg(long)]
    json: bool,
    /// Suppress progress, retaining errors and completion summary
    #[arg(short, long)]
    quiet: bool,
}

#[derive(Args)]
struct Download {
    url: String,
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Expected SHA-256; checked before publishing the completed output
    #[arg(long, value_parser = checksum)]
    sha256: Option<String>,
    /// Replace an existing completed output only after the new file verifies
    #[arg(long)]
    force: bool,
    /// Skip HTML link resolution (download the response itself)
    #[arg(long)]
    direct: bool,
    /// Choose a 1-based candidate number; useful for scripts and --json
    #[arg(long = "select", conflicts_with = "direct", value_parser = clap::value_parser!(u16).range(1..=200))]
    selection: Option<u16>,
    /// Show the arrow-key picker even when only one candidate is found
    #[arg(long, conflicts_with = "direct")]
    pick: bool,
    #[command(flatten)]
    network: Network,
    #[command(flatten)]
    tuning: Tuning,
}

fn checksum(value: &str) -> Result<String, String> {
    if value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit()) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err("SHA-256 must contain exactly 64 hexadecimal characters".into())
    }
}

fn external(program: &str, args: &[OsString]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("Install {program} and add it to PATH; run 'fluxget doctor'"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("fluxget: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|s| {
        s.to_string_lossy().starts_with("https://") || s.to_string_lossy().starts_with("http://")
    }) {
        args.insert(1, "get".into());
    }
    match Cli::parse_from(args).command {
        Action::Get(d) => {
            let client = http::client(&d.network)?;
            let url = if d.direct {
                http::valid_url(&d.url)?
            } else {
                let Some(url) = resolver::select(
                    &client,
                    &d.url,
                    d.selection.map(usize::from),
                    d.pick,
                    d.tuning.json,
                )
                .await?
                else {
                    eprintln!("Cancelled; no download started.");
                    return Ok(());
                };
                url
            };
            let info = http::probe(&client, url, d.tuning.retries).await?;
            let output = d.output.unwrap_or_else(|| PathBuf::from(&info.filename));
            engine::download(client, info, output, d.sha256, d.force, &d.tuning).await?;
        }
        Action::Resume {
            output,
            network,
            tuning,
        } => {
            let checkpoint = state::load(&state::sidecar(&output, ".fluxget.json"))?;
            let client = http::client(&network)?;
            let info =
                http::probe(&client, http::valid_url(&checkpoint.url)?, tuning.retries).await?;
            engine::download(
                client,
                info,
                output,
                checkpoint.expected_sha256,
                false,
                &tuning,
            )
            .await?;
        }
        Action::Inspect { url, network } => {
            let info = http::probe(&http::client(&network)?, http::valid_url(&url)?, 2).await?;
            println!("{}", serde_json::to_string_pretty(&info)?);
        }
        Action::Resolve { url, network } => {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &resolver::discover(&http::client(&network)?, &url).await?
                )?
            );
        }
        Action::Doctor => {
            println!(
                "fluxget {} | {}-{} | native HTTP(S) ready",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            for program in ["aria2c", "yt-dlp", "curl"] {
                match Command::new(program).arg("--version").output() {
                    Ok(o) if o.status.success() => println!(
                        "{program}: {}",
                        String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .next()
                            .unwrap_or("available")
                    ),
                    _ => println!("{program}: optional, not found"),
                }
            }
        }
        Action::Aria2 { args } => external("aria2c", &args)?,
        Action::Media { args } => external("yt-dlp", &args)?,
    }
    Ok(())
}
