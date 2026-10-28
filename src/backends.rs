//! Delegation to mature open-source tools for what the native HTTP engine does
//! not do: BitTorrent (aria2) and video/audio sites (yt-dlp). Arguments are
//! passed as a process argument vector; no shell command is ever constructed.
use crate::{
    http,
    mediaui::{self, MediaTelemetry},
    ui, Network,
};
use anyhow::{bail, ensure, Context, Result};
use reqwest::Client;
use serde::Serialize;
use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Serialize)]
struct Outcome<'a> {
    backend: &'a str,
    kind: &'a str,
    source: String,
    directory: &'a Path,
    status: &'a str,
}

/// Run an external program. With `json` its stdout is redirected to stderr so
/// that stdout carries exactly one machine-readable result.
pub fn run(program: &str, args: &[OsString], json: bool) -> Result<()> {
    let mut command = Command::new(program);
    command.args(args);
    if json {
        command.stdout(Stdio::from(std::io::stderr()));
    }
    let status = command.status().with_context(|| {
        format!(
            "Install {} and add it to PATH; run 'fluxget doctor'",
            program_name(program)
        )
    })?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

fn program_name(program: &str) -> &str {
    match program {
        "aria2c" => "aria2 (https://aria2.github.io/)",
        "yt-dlp" => "yt-dlp (https://github.com/yt-dlp/yt-dlp)",
        "winget" => "winget (the Windows Package Manager, included with Windows 10 and 11)",
        other => other,
    }
}

fn directory(output: Option<&Path>) -> PathBuf {
    output.map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn report(backend: &str, kind: &str, source: &str, dir: &Path, json: bool) -> Result<()> {
    if json {
        let source = crate::sites::short(source);
        println!(
            "{}",
            serde_json::to_string(&Outcome {
                backend,
                kind,
                source,
                directory: dir,
                status: "completed"
            })?
        );
    }
    Ok(())
}

/// A bencoded dictionary containing an `info` entry.
fn is_torrent(data: &[u8]) -> bool {
    data.first() == Some(&b'd') && data.windows(6).any(|w| w == b"4:info")
}

/// A `.torrent` file is fetched with the same client (proxy, headers, limits)
/// as everything else and handed to aria2 as a local file.
async fn torrent_file(client: &Client, url: &str) -> Result<tempfile::TempPath> {
    let url = http::valid_url(url)?;
    let (mut response, _) = http::request(client.get(url), 2).await?;
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            data.len() + chunk.len() <= 16 << 20,
            "Torrent file exceeds 16 MiB"
        );
        data.extend_from_slice(&chunk);
    }
    ensure!(
        is_torrent(&data),
        "The download is not a valid .torrent file (a login or error page?)"
    );
    let mut file = tempfile::Builder::new().suffix(".torrent").tempfile()?;
    file.write_all(&data)?;
    Ok(file.into_temp_path())
}

/// Download the contents of a torrent: a magnet link, a `.torrent` URL, or a
/// local `.torrent` file. Peers, piece hashes and resume belong to aria2.
pub async fn torrent(
    client: &Client,
    source: &str,
    output: Option<&Path>,
    seed_minutes: u32,
    quiet: bool,
    json: bool,
) -> Result<()> {
    let dir = directory(output);
    let fetched;
    let input: OsString = if source.starts_with("http://") || source.starts_with("https://") {
        fetched = torrent_file(client, source).await?;
        fetched.as_os_str().to_owned()
    } else {
        source.into()
    };
    let mut args: Vec<OsString> = vec![
        "--dir".into(),
        dir.clone().into_os_string(),
        format!("--seed-time={seed_minutes}").into(),
        "--file-allocation=none".into(),
        "--check-integrity=true".into(),
        "--auto-file-renaming=false".into(),
        "--allow-overwrite=false".into(),
        "--console-log-level=warn".into(),
    ];
    if quiet || json {
        args.push("--quiet=true".into());
    }
    args.push(input);
    if !quiet && !json {
        eprintln!(
            "Downloading torrent with aria2 into {} (seeding {})",
            dir.display(),
            if seed_minutes == 0 {
                "off".into()
            } else {
                format!("{seed_minutes} min")
            }
        );
    }
    run("aria2c", &args, json)?;
    report("aria2", "torrent", source, &dir, json)
}

fn has_program(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// YouTube serves each stream in 10 MiB requests and throttles long ones, so splitting a
/// stream across aria2 connections is slower there (measured: about half the speed of
/// yt-dlp's own downloader). Its fragmented formats are still fetched concurrently.
fn throttles_split_downloads(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| {
            ["youtube.com", "youtu.be", "youtube-nocookie.com"]
                .iter()
                .any(|site| host == *site || host.ends_with(&format!(".{site}")))
        })
}

/// One stream is often throttled per connection (most CDNs). yt-dlp fetches fragmented
/// HLS/DASH formats concurrently itself; plain files go through aria2 when it is
/// installed, which splits them across connections. The downloader is named without a
/// protocol: yt-dlp matches 'https:aria2c' against the shortened name 'http' and silently
/// keeps its own single-connection downloader. aria2 declines the protocols it cannot
/// handle, so those still fall back to yt-dlp's.
fn parallel_args(aria2: bool, url: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--concurrent-fragments".into(), "16".into()];
    if aria2 && !throttles_split_downloads(url) {
        args.extend([
            "--downloader".into(),
            "aria2c".into(),
            "--downloader-args".into(),
            "aria2c:-x16 -s16 -k1M".into(),
        ]);
    }
    args
}

/// Lines kept from yt-dlp's standard error while the display covers the terminal.
const KEPT_ERRORS: usize = 200;

/// Runs yt-dlp with its output piped into the live display, and returns its exit status and
/// everything it wrote to standard error. A Ctrl+C reaches yt-dlp too (same console); if it
/// has not exited a few seconds later it is killed.
async fn run_displayed(
    args: &[OsString],
    telemetry: Arc<MediaTelemetry>,
) -> Result<(ExitStatus, Vec<String>, bool)> {
    let mut command = Command::new("yt-dlp");
    command
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().with_context(|| {
        format!(
            "Install {} and add it to PATH; run 'fluxget doctor'",
            program_name("yt-dlp")
        )
    })?;
    let stdout = child.stdout.take().context("yt-dlp gave no output")?;
    let stderr = child.stderr.take().context("yt-dlp gave no error output")?;
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = stop.clone();
        tokio::task::spawn_blocking(move || -> Result<(ExitStatus, Vec<String>)> {
            let progress = std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut line = Vec::new();
                while reader.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                    telemetry.feed(&String::from_utf8_lossy(&line));
                    line.clear();
                }
            });
            let errors = std::thread::spawn(move || {
                let mut kept = Vec::new();
                let mut reader = BufReader::new(stderr);
                let mut line = Vec::new();
                while reader.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                    if kept.len() < KEPT_ERRORS {
                        kept.push(String::from_utf8_lossy(&line).trim_end().to_owned());
                    }
                    line.clear();
                }
                kept
            });
            let mut stopped = None;
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if stop.load(Ordering::Relaxed) {
                    let since = *stopped.get_or_insert_with(Instant::now);
                    if since.elapsed() > Duration::from_secs(3) {
                        let _ = child.kill();
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            let _ = progress.join();
            Ok((status, errors.join().unwrap_or_default()))
        })
    };
    tokio::pin!(worker);
    let mut interrupted = false;
    let cancel = tokio::signal::ctrl_c();
    tokio::pin!(cancel);
    let result = loop {
        tokio::select! {
            result = &mut worker => break result.context("yt-dlp monitor stopped")??,
            _ = &mut cancel, if !interrupted => {
                interrupted = true;
                stop.store(true, Ordering::Relaxed);
            }
        }
    };
    Ok((result.0, result.1, interrupted))
}

/// Download a video or audio page with yt-dlp (a single item, not a playlist). In a terminal
/// its output is replaced by the same live display plain downloads get.
pub async fn media(
    url: &str,
    output: Option<&Path>,
    network: &Network,
    quiet: bool,
    json: bool,
) -> Result<()> {
    let dir = directory(output);
    let mut args: Vec<OsString> = vec![
        "--no-playlist".into(),
        "--paths".into(),
        dir.clone().into_os_string(),
        "--no-overwrites".into(),
    ];
    if let Some(proxy) = &network.proxy {
        args.push("--proxy".into());
        args.push(proxy.into());
    }
    args.extend(parallel_args(has_program("aria2c"), url));
    if let Some(browser) = &network.cookies_from_browser {
        args.push("--cookies-from-browser".into());
        args.push(browser.into());
    }
    if quiet || json {
        args.push("--quiet".into());
        args.push("--no-warnings".into());
    }
    let live = !quiet && !json && ui::interactive();
    if live {
        args.extend(mediaui::TEMPLATES.iter().map(OsString::from));
    }
    args.push("--".into());
    args.push(url.into());
    let hint = |error: anyhow::Error| {
        if network.cookies_from_browser.is_some() {
            return error;
        }
        error.context(
            "If the site asked you to sign in or confirm you are not a bot, retry with \
             'fluxget get --cookies-from-browser <chrome|firefox|safari|edge> <url>'; \
             'yt-dlp -U' or 'brew upgrade yt-dlp' also fixes many extraction errors",
        )
    };
    if !live {
        run("yt-dlp", &args, json).map_err(hint)?;
        return report("yt-dlp", "media", url, &dir, json);
    }
    let source = url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_owned())
        })
        .unwrap_or_else(|| "media".to_owned());
    let telemetry = Arc::new(MediaTelemetry::new(source.clone()));
    let started = Instant::now();
    let display = {
        let telemetry = telemetry.clone();
        let mut state = ui::State::new();
        ui::Display::spawn(move |rows, cols| mediaui::render(&telemetry, &mut state, rows, cols))
    };
    let outcome = run_displayed(&args, telemetry.clone()).await;
    // Clear the display first, so what follows starts on a clean line.
    drop(display);
    let (status, errors, interrupted) = outcome.map_err(hint)?;
    for line in errors.iter().filter(|l| !l.is_empty()) {
        eprintln!("{line}");
    }
    if interrupted {
        bail!("Interrupted; run the same command again to resume the partial download");
    }
    if !status.success() {
        return Err(hint(anyhow::anyhow!("yt-dlp exited with {status}")));
    }
    let done = telemetry.snapshot();
    let (bytes, _, received, resumed) = done.totals();
    let elapsed = started.elapsed().as_secs_f64();
    let title = done.title.clone().unwrap_or(source);
    let place = done
        .path
        .clone()
        .unwrap_or_else(|| dir.display().to_string());
    let rate = if received > 0 {
        format!(
            "   {:.1} MiB/s",
            received as f64 / 1048576.0 / elapsed.max(0.000001)
        )
    } else {
        String::new()
    };
    let streams = match done.streams.len() {
        0 | 1 => String::new(),
        n => format!("   ·   {n} streams merged"),
    };
    let resumed = if resumed > 0 {
        format!("   resumed {}", ui::bytes(resumed))
    } else {
        String::new()
    };
    println!(
        "\n  \x1b[32m✔\x1b[0m  \x1b[1;97mDownloaded\x1b[0m  {title}\n     \x1b[90m{place}\x1b[0m\n     {} in {}{rate}{streams}{resumed}\n",
        ui::bytes(bytes),
        ui::duration(elapsed),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_program_gives_actionable_error() {
        let error = run("fluxget-definitely-not-installed", &[], false).unwrap_err();
        assert!(format!("{error:#}").contains("fluxget doctor"));
    }

    #[test]
    fn media_downloads_in_parallel_with_or_without_aria2() {
        let site = "https://example.com/watch/1";
        let plain = parallel_args(false, site);
        assert_eq!(plain, ["--concurrent-fragments", "16"]);
        let fast = parallel_args(true, site);
        let position = fast.iter().position(|a| a == "--downloader").unwrap();
        assert_eq!(fast[position + 1], "aria2c");
        assert!(fast.iter().any(|a| a == "aria2c:-x16 -s16 -k1M"));
    }

    #[test]
    fn youtube_keeps_its_own_downloader() {
        for url in [
            "https://youtu.be/LVNfb5DC-C4?si=x",
            "https://www.youtube.com/watch?v=LVNfb5DC-C4",
            "https://music.youtube.com/watch?v=a",
        ] {
            assert_eq!(
                parallel_args(true, url),
                ["--concurrent-fragments", "16"],
                "{url}"
            );
        }
        assert!(!throttles_split_downloads("https://notyoutube.com/x"));
    }

    #[test]
    fn only_bencoded_torrent_metadata_is_accepted() {
        assert!(is_torrent(b"d8:announce3:url4:infod4:name1:aee"));
        assert!(!is_torrent(b"<html>login required</html>"));
        assert!(!is_torrent(b"d3:foo3:bare"));
        assert_eq!(directory(None), PathBuf::from("."));
    }
}
