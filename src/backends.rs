//! Delegation to mature open-source tools for what the native HTTP engine does
//! not do: BitTorrent (aria2) and video/audio sites (yt-dlp). Arguments are
//! passed as a process argument vector; no shell command is ever constructed.
use crate::{http, Network};
use anyhow::{bail, ensure, Context, Result};
use reqwest::Client;
use serde::Serialize;
use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
    let name = match program {
        "aria2c" => "aria2 (https://aria2.github.io/)",
        "yt-dlp" => "yt-dlp (https://github.com/yt-dlp/yt-dlp)",
        other => other,
    };
    let status = command
        .status()
        .with_context(|| format!("Install {name} and add it to PATH; run 'fluxget doctor'"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
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

/// Download a video or audio page with yt-dlp (a single item, not a playlist).
pub fn media(
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
    if quiet || json {
        args.push("--quiet".into());
        args.push("--no-warnings".into());
    }
    args.push("--".into());
    args.push(url.into());
    run("yt-dlp", &args, json)?;
    report("yt-dlp", "media", url, &dir, json)
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
    fn only_bencoded_torrent_metadata_is_accepted() {
        assert!(is_torrent(b"d8:announce3:url4:infod4:name1:aee"));
        assert!(!is_torrent(b"<html>login required</html>"));
        assert!(!is_torrent(b"d3:foo3:bare"));
        assert_eq!(directory(None), PathBuf::from("."));
    }
}
