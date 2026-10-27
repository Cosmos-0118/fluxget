use crate::{
    http::{self, Info},
    state::{self, Checkpoint},
    Tuning,
};
use anyhow::{bail, ensure, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{Client, StatusCode};
use serde::Serialize;
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::task::JoinSet;

#[derive(Serialize)]
struct Summary {
    output: PathBuf,
    bytes: u64,
    resumed_bytes: u64,
    transferred_bytes: u64,
    elapsed_seconds: f64,
    mib_per_second: f64,
    sha256: String,
    strategy: &'static str,
    peak_connections: usize,
    concurrency_changes: usize,
}

/// Additive exploration, rollback on no gain, multiplicative decrease on throttle.
struct Controller {
    target: usize,
    max: usize,
    fixed: bool,
    last_rate: Option<f64>,
    hold: u8,
    changes: usize,
    peak: usize,
}

impl Controller {
    fn new(t: &Tuning) -> Self {
        let max = t.connections as usize;
        let target = if t.fixed { max } else { max.min(2) };
        Self {
            target,
            max,
            fixed: t.fixed,
            last_rate: None,
            hold: 0,
            changes: 0,
            peak: target,
        }
    }
    fn update(&mut self, rate: f64, throttled: bool) {
        let previous = self.target;
        if throttled {
            self.target = (self.target / 2).max(1);
            self.last_rate = None;
            self.hold = 3;
        } else if !self.fixed && rate > 0.0 {
            if self.hold > 0 {
                self.hold -= 1;
            } else if self.last_rate.is_none_or(|last| rate > last * 1.05) {
                if self.target < self.max {
                    self.target += 1;
                }
                self.last_rate = Some(rate);
            } else {
                self.target = self.target.saturating_sub(1).max(1);
                self.last_rate = None;
                self.hold = 3;
            }
        }
        if previous != self.target {
            self.changes += 1;
        }
        self.peak = self.peak.max(self.target);
    }
}

async fn fetch_chunk(
    client: Client,
    info: Info,
    index: usize,
    chunk_size: u64,
    retries: u8,
) -> Result<(usize, Vec<u8>, bool)> {
    let size = info.size.context("Range download requires size")?;
    let start = index as u64 * chunk_size;
    let end = (size - start).min(chunk_size) + start - 1;
    let expected = (end - start + 1) as usize;
    let mut throttled = false;
    for attempt in 0..=retries {
        let builder = client
            .get(&info.url)
            .header("range", format!("bytes={start}-{end}"))
            .header(
                "if-match",
                info.etag
                    .as_deref()
                    .context("Range download requires strong ETag")?,
            );
        // Header-level retries respect Retry-After. Body failures replay only this chunk.
        let (mut response, was_throttled) = http::request(builder, retries).await?;
        throttled |= was_throttled;
        ensure!(response.status() == StatusCode::PARTIAL_CONTENT, "Server stopped honoring ranges; partial data retained. Retry with a new output and --single-stream");
        http::identity(&response)?;
        ensure!(
            http::range(
                http::header(&response, "content-range").context("Missing Content-Range")?
            )? == (start, end, size),
            "Server returned the wrong Content-Range"
        );
        if let Some(etag) = http::header(&response, "etag") {
            ensure!(
                Some(etag) == info.etag.as_deref(),
                "Remote ETag changed during download"
            );
        }
        let mut data = Vec::with_capacity(expected);
        let mut body_error = None;
        loop {
            match response.chunk().await {
                Ok(Some(bytes)) => {
                    ensure!(
                        data.len() + bytes.len() <= expected,
                        "Range body exceeds requested size"
                    );
                    data.extend_from_slice(&bytes);
                }
                Ok(None) => break,
                Err(e) => {
                    body_error = Some(e);
                    break;
                }
            }
        }
        if body_error.is_none() && data.len() == expected {
            return Ok((index, data, throttled));
        }
        if attempt == retries {
            bail!("Range {start}-{end} truncated after retries ({} of {expected} bytes): {body_error:?}", data.len());
        }
        tokio::time::sleep(Duration::from_millis(250 * (1u64 << attempt))).await;
    }
    unreachable!()
}

fn checkpoint(info: &Info, expected: Option<String>, chunk_size: u64) -> Result<Checkpoint> {
    let size = info.size.context("Missing size")?;
    let count = size.div_ceil(chunk_size);
    ensure!(
        count <= 400_000,
        "Too many chunks; increase --chunk-kib (maximum 400000 chunks)"
    );
    Ok(Checkpoint {
        version: 1,
        url: info.url.clone(),
        size,
        etag: info.etag.clone(),
        expected_sha256: expected,
        chunk_size,
        chunks: vec![None; count as usize],
    })
}

async fn segmented(
    client: &Client,
    info: &Info,
    file: &mut File,
    checkpoint_path: &Path,
    state: &mut Checkpoint,
    tuning: &Tuning,
    progress: &ProgressBar,
) -> Result<(u64, Controller)> {
    let resumed = state::validate_parts(file, state)?;
    file.set_len(state.size)?;
    state::save(checkpoint_path, state)?;
    progress.set_position(resumed);
    let mut queue: VecDeque<usize> = state
        .chunks
        .iter()
        .enumerate()
        .filter_map(|(i, done)| done.is_none().then_some(i))
        .collect();
    let mut tasks = JoinSet::new();
    let mut controller = Controller::new(tuning);
    let mut sample_start = Instant::now();
    let mut sample_bytes = 0u64;
    let cancel = tokio::signal::ctrl_c();
    tokio::pin!(cancel);
    while !queue.is_empty() || !tasks.is_empty() {
        while tasks.len() < controller.target {
            let Some(index) = queue.pop_front() else {
                break;
            };
            tasks.spawn(fetch_chunk(
                client.clone(),
                info.clone(),
                index,
                state.chunk_size,
                tuning.retries,
            ));
        }
        tokio::select! {
            _ = &mut cancel => bail!("Interrupted; run 'fluxget resume <output>' to continue"),
            result = tasks.join_next() => {
                let (index, bytes, throttled) = result.context("No worker result")?? ?;
                file.seek(SeekFrom::Start(index as u64 * state.chunk_size))?;
                file.write_all(&bytes)?;
                // Data reaches stable storage before we claim this chunk in the journal.
                file.sync_data()?;
                state.chunks[index] = Some(state::hash(&bytes));
                state::save(checkpoint_path, state)?;
                progress.inc(bytes.len() as u64);
                sample_bytes += bytes.len() as u64;
                let elapsed = sample_start.elapsed().as_secs_f64();
                if throttled || elapsed >= 0.75 {
                    controller.update(sample_bytes as f64 / elapsed.max(0.001), throttled);
                    progress.set_message(format!("{} workers", controller.target));
                    sample_start = Instant::now();
                    sample_bytes = 0;
                }
            }
        }
    }
    Ok((resumed, controller))
}

async fn single(
    client: &Client,
    info: &Info,
    file: &mut File,
    tuning: &Tuning,
    progress: &ProgressBar,
) -> Result<u64> {
    let cancel = tokio::signal::ctrl_c();
    tokio::pin!(cancel);
    for attempt in 0..=tuning.retries {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        progress.set_position(0);
        let builder = client.get(&info.url);
        let builder = if let Some(etag) = &info.etag {
            builder.header("if-match", etag)
        } else {
            builder
        };
        let (mut response, _) = tokio::select! {
            _ = &mut cancel => bail!("Interrupted; this server requires a fresh single-stream transfer"),
            result = http::request(builder, tuning.retries) => result?,
        };
        ensure!(
            response.status() == StatusCode::OK,
            "Unexpected single-stream status {}",
            response.status()
        );
        http::identity(&response)?;
        if let Some(etag) = http::header(&response, "etag") {
            ensure!(
                info.etag.as_deref().is_none_or(|e| e == etag),
                "Remote ETag changed"
            );
        }
        let mut count = 0u64;
        let mut failure = None;
        loop {
            let next = tokio::select! {
                _ = &mut cancel => bail!("Interrupted; this server requires a fresh single-stream transfer"),
                result = response.chunk() => result,
            };
            match next {
                Ok(Some(bytes)) => {
                    count += bytes.len() as u64;
                    ensure!(
                        info.size.is_none_or(|size| count <= size),
                        "Response exceeds probed size"
                    );
                    file.write_all(&bytes)?;
                    progress.inc(bytes.len() as u64);
                }
                Ok(None) => break,
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        if failure.is_none() && info.size.is_none_or(|size| size == count) {
            return Ok(count);
        }
        if attempt == tuning.retries {
            bail!("Single stream truncated after retries: {failure:?}");
        }
        tokio::time::sleep(Duration::from_millis(250 * (1u64 << attempt))).await;
    }
    unreachable!()
}

fn publish(part: &Path, output: &Path, force: bool) -> Result<()> {
    let path = tempfile::TempPath::try_from_path(part)?;
    let result = if force {
        path.persist(output)
    } else {
        path.persist_noclobber(output)
    };
    if let Err(error) = result {
        let cause = error.error;
        error
            .path
            .keep()
            .context("Could not preserve completed partial file")?;
        return Err(cause).context("Cannot publish output; completed partial file preserved");
    }
    Ok(())
}

pub async fn download(
    client: Client,
    info: Info,
    output: PathBuf,
    expected: Option<String>,
    force: bool,
    tuning: &Tuning,
) -> Result<()> {
    ensure!(
        !output.exists() || force,
        "Output already exists; choose another -o path or use --force"
    );
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let _lock = state::lock(&output)?;
    let part = state::sidecar(&output, ".fluxget.part");
    let checkpoint_path = state::sidecar(&output, ".fluxget.json");
    // A missing strong validator makes safe range composition impossible.
    let use_ranges = !tuning.single_stream
        && info.ranges
        && info.etag.is_some()
        && info.size.is_some_and(|n| n > 0);
    let mut journal = if checkpoint_path.exists() {
        let loaded = state::load(&checkpoint_path)?;
        ensure!(use_ranges && loaded.url == info.url && Some(loaded.size) == info.size && loaded.etag == info.etag && loaded.expected_sha256 == expected,
            "Remote identity or checksum changed; preserve/delete the .fluxget.part and .fluxget.json files, then restart");
        ensure!(
            part.exists(),
            "Checkpoint exists but partial file is missing"
        );
        Some(loaded)
    } else if use_ranges {
        ensure!(
            !part.exists(),
            "Untracked partial file exists; move/delete it before restarting"
        );
        Some(checkpoint(
            &info,
            expected.clone(),
            tuning.chunk_kib as u64 * 1024,
        )?)
    } else {
        None
    };
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&part)?;
    let progress = if tuning.quiet || tuning.json {
        ProgressBar::hidden()
    } else {
        ProgressBar::new(info.size.unwrap_or(0))
    };
    progress.set_style(ProgressStyle::with_template("{spinner:.cyan} {bytes}/{total_bytes} [{wide_bar:.cyan/blue}] {bytes_per_sec} ETA {eta} {msg}")?);
    progress.enable_steady_tick(Duration::from_millis(200));
    let started = Instant::now();
    let (bytes, resumed, peak, changes, strategy) = if let Some(journal) = journal.as_mut() {
        let (resumed, controller) = segmented(
            &client,
            &info,
            &mut file,
            &checkpoint_path,
            journal,
            tuning,
            &progress,
        )
        .await?;
        (
            journal.size,
            resumed,
            controller.peak,
            controller.changes,
            if tuning.fixed {
                "fixed-ranges"
            } else {
                "adaptive-ranges"
            },
        )
    } else {
        let bytes = single(&client, &info, &mut file, tuning, &progress).await?;
        (bytes, 0, 1, 0, "single-stream")
    };
    file.sync_all()?;
    drop(file);
    progress.set_message("verifying SHA-256");
    let hash = state::hash_file(&part)?;
    if let Some(expected) = expected {
        ensure!(
            hash == expected,
            "SHA-256 mismatch: expected {expected}, got {hash}; partial file retained"
        );
    }
    publish(&part, &output, force)?;
    if checkpoint_path.exists() {
        fs::remove_file(checkpoint_path)?;
    }
    progress.finish_and_clear();
    let elapsed = started.elapsed().as_secs_f64();
    let summary = Summary {
        output,
        bytes,
        resumed_bytes: resumed,
        transferred_bytes: bytes - resumed,
        elapsed_seconds: elapsed,
        mib_per_second: (bytes - resumed) as f64 / 1048576.0 / elapsed.max(0.000001),
        sha256: hash,
        strategy,
        peak_connections: peak,
        concurrency_changes: changes,
    };
    if tuning.json {
        println!("{}", serde_json::to_string(&summary)?);
    } else {
        println!(
            "{}: {} bytes in {:.2}s ({:.2} MiB/s), resumed {} bytes\nSHA-256 {}",
            summary.output.display(),
            bytes,
            elapsed,
            summary.mib_per_second,
            resumed,
            summary.sha256
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adaptive_caps_and_throttle() {
        let t = Tuning {
            connections: 8,
            fixed: false,
            single_stream: false,
            chunk_kib: 2048,
            retries: 4,
            json: false,
            quiet: true,
        };
        let mut c = Controller::new(&t);
        c.update(100.0, false);
        assert_eq!(c.target, 3);
        c.update(101.0, false);
        assert_eq!(c.target, 2);
        c.update(100.0, true);
        assert_eq!(c.target, 1);
        for n in 1..100 {
            c.update(n as f64 * 100.0, false);
            assert!((1..=8).contains(&c.target));
        }
    }
    #[test]
    fn damaged_chunk_is_redownloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("part");
        let mut file = File::create(&path).unwrap();
        file.write_all(&vec![5; 65536]).unwrap();
        drop(file);
        let mut cp = Checkpoint {
            version: 1,
            url: "https://example.com".into(),
            size: 65536,
            etag: Some("\"v1\"".into()),
            expected_sha256: None,
            chunk_size: 65536,
            chunks: vec![Some(state::hash(&vec![5; 65536]))],
        };
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        assert_eq!(state::validate_parts(&mut file, &mut cp).unwrap(), 65536);
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&[0]).unwrap();
        assert_eq!(state::validate_parts(&mut file, &mut cp).unwrap(), 0);
        assert!(cp.chunks[0].is_none());
    }
}
