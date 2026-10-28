mod backends;
mod engine;
mod forges;
mod http;
mod location;
mod picker;
mod resolver;
mod sites;
mod state;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

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
    /// Download a file, or choose a detected page download with arrow keys.
    /// Understands GitHub/GitLab/Codeberg releases, Hugging Face, archive.org,
    /// SourceForge, Drive/Dropbox/OneDrive links, torrents and media pages
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
    /// Run the Windows Package Manager, e.g. 'fluxget winget install BurntSushi.ripgrep.MSVC'
    #[command(trailing_var_arg = true)]
    Winget {
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
    #[arg(short = 'j', long, default_value_t = 16, value_parser = clap::value_parser!(u8).range(1..=32))]
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
    /// Page, file or release URL; a magnet link; or a local .torrent file
    url: String,
    /// Output file. For torrents and media pages: the output directory.
    /// Without it, files go to your Downloads folder (a terminal asks first)
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Minutes to keep seeding after a torrent completes (0 stops immediately)
    #[arg(long, default_value_t = 0)]
    seed_minutes: u32,
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

/// A bare argument that names something downloadable implies the `get` command.
fn implies_get(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("magnet:")
        || (lower.ends_with(".torrent") && Path::new(arg).is_file())
        || http::normalize(arg) != arg
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
    if args
        .get(1)
        .is_some_and(|s| implies_get(&s.to_string_lossy()))
    {
        args.insert(1, "get".into());
    }
    match Cli::parse_from(args).command {
        Action::Get(d) => {
            let client = http::client(&d.network)?;
            let target = if d.direct {
                resolver::Target {
                    kind: resolver::Kind::Http,
                    url: http::valid_url(&d.url)?.to_string(),
                    sha256: None,
                    info: None,
                }
            } else {
                let Some(target) = resolver::select(
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
                target
            };
            // Probe the chosen file while the person answers the destination question.
            let pending =
                (target.kind == resolver::Kind::Http && target.info.is_none()).then(|| {
                    let (client, url, retries) =
                        (client.clone(), target.url.clone(), d.tuning.retries);
                    tokio::spawn(async move {
                        http::probe(&client, http::valid_url(&url)?, retries).await
                    })
                });
            let folder = if d.output.is_some() {
                None
            } else if !picker::headless(d.tuning.json) && !d.tuning.quiet {
                match location::ask()? {
                    Some(folder) => Some(folder),
                    None => {
                        if let Some(task) = pending {
                            task.abort();
                        }
                        eprintln!("Cancelled; no download started.");
                        return Ok(());
                    }
                }
            } else {
                Some(location::downloads_dir())
            };
            match target.kind {
                resolver::Kind::Torrent => {
                    backends::torrent(
                        &client,
                        &target.url,
                        d.output.as_deref().or(folder.as_deref()),
                        d.seed_minutes,
                        d.tuning.quiet,
                        d.tuning.json,
                    )
                    .await?
                }
                resolver::Kind::Media => backends::media(
                    &target.url,
                    d.output.as_deref().or(folder.as_deref()),
                    &d.network,
                    d.tuning.quiet,
                    d.tuning.json,
                )?,
                resolver::Kind::Http => {
                    let info = match (target.info, pending) {
                        (Some(info), _) => info,
                        (None, Some(task)) => task.await.context("File probe stopped")??,
                        (None, None) => unreachable!("an unprobed HTTP target is always probed"),
                    };
                    let output = match (d.output, folder) {
                        (Some(output), _) => output,
                        (None, Some(folder)) if d.force => folder.join(&info.filename),
                        (None, Some(folder)) => location::free_name(&folder, &info.filename),
                        (None, None) => PathBuf::from(&info.filename),
                    };
                    let expected = d.sha256.or(target.sha256);
                    engine::download(client, info, output, expected, d.force, &d.tuning).await?;
                }
            }
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
            let tools: &[&str] = if cfg!(windows) {
                &["aria2c", "yt-dlp", "curl", "winget"]
            } else {
                &["aria2c", "yt-dlp", "curl"]
            };
            for &program in tools {
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
        Action::Aria2 { args } => backends::run("aria2c", &args, false)?,
        Action::Media { args } => backends::run("yt-dlp", &args, false)?,
        Action::Winget { args } => backends::run("winget", &args, false)?,
    }
    Ok(())
}
