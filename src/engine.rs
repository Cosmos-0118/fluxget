use crate::{
    http::{self, Info},
    paused,
    state::{self, Checkpoint},
    ui::{self, Telemetry},
    Tuning,
};
use anyhow::{bail, ensure, Context, Result};
use reqwest::{Client, StatusCode};
use serde::Serialize;
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
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
    /// What guarded the file against changing: etag, last-modified, weak-etag or none
    /// (spot-checked at three places instead).
    validator: &'static str,
    peak_connections: usize,
    concurrency_changes: usize,
}

/// Fast multiplicative ramp-up while throughput keeps improving, rollback and a
/// pause when it does not, multiplicative decrease on throttling. Samples count
/// bytes as they arrive, not only finished chunks, so they are not quantized.
struct Controller {
    target: usize,
    max: usize,
    fixed: bool,
    last_rate: Option<f64>,
    before_growth: usize,
    strikes: u8,
    hold: u8,
    changes: usize,
    peak: usize,
}

/// Samples to skip after a change: new connections need time to ramp up.
const SETTLE_SAMPLES: u8 = 1;
/// Samples to wait after a plateau before probing for more throughput again.
const PLATEAU_SAMPLES: u8 = 10;

impl Controller {
    fn new(t: &Tuning) -> Self {
        let max = t.connections as usize;
        let target = if t.fixed { max } else { max.min(4) };
        Self {
            target,
            max,
            fixed: t.fixed,
            last_rate: None,
            before_growth: target,
            strikes: 0,
            hold: 0,
            changes: 0,
            peak: target,
        }
    }
    fn grow(&mut self) {
        self.before_growth = self.target;
        self.target = (self.target + (self.target / 2).max(1)).min(self.max);
        self.hold = SETTLE_SAMPLES;
    }
    fn throttled(&mut self) {
        let previous = self.target;
        self.target = (self.target / 2).max(1);
        self.last_rate = None;
        self.hold = 3;
        self.record(previous);
    }
    fn sample(&mut self, rate: f64) {
        if self.fixed || rate <= 0.0 {
            return;
        }
        let previous = self.target;
        if self.hold > 0 {
            self.hold -= 1;
        } else if self.last_rate.is_none_or(|last| rate > last * 1.10) {
            self.strikes = 0;
            self.last_rate = Some(rate);
            if self.target < self.max {
                self.grow();
            }
        } else {
            // One flat sample may be noise; two in a row is a plateau.
            self.strikes += 1;
            if self.strikes >= 2 {
                self.strikes = 0;
                self.target = self.before_growth.min(self.target).max(1);
                self.last_rate = None;
                self.hold = PLATEAU_SAMPLES;
            }
        }
        self.record(previous);
    }
    fn record(&mut self, previous: usize) {
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
    telemetry: Arc<Telemetry>,
) -> Result<(usize, Vec<u8>, bool)> {
    let size = info.size.context("Range download requires size")?;
    let start = index as u64 * chunk_size;
    let end = (size - start).min(chunk_size) + start - 1;
    let expected = (end - start + 1) as usize;
    let received = telemetry.received.clone();
    let lane = telemetry.begin_lane(index, expected as u64);
    let mut throttled = false;
    for attempt in 0..=retries {
        lane.lane.done.store(0, Ordering::Relaxed);
        let builder = info.guard(
            client
                .get(&info.url)
                .header("range", format!("bytes={start}-{end}")),
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
        info.unchanged(&response)?;
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
                    received.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    lane.lane
                        .done
                        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
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
        received.fetch_sub(data.len() as u64, Ordering::Relaxed);
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
        weak_etag: info.weak_etag.clone(),
        last_modified: info.last_modified.clone(),
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
    telemetry: &Arc<Telemetry>,
) -> Result<(u64, Controller)> {
    let resumed = state::validate_parts(file, state)?;
    if resumed > 0 && info.etag.is_none() {
        // Without a strong ETag a remote change between sessions would splice two versions.
        spot_check(client, info, file, &spans(state)).await?;
    }
    file.set_len(state.size)?;
    state::save(checkpoint_path, state)?;
    telemetry.resumed.store(resumed, Ordering::Relaxed);
    for (index, hash) in state.chunks.iter().enumerate() {
        if hash.is_some() {
            telemetry.mark(index, ui::DONE);
        }
    }
    let mut queue: VecDeque<usize> = state
        .chunks
        .iter()
        .enumerate()
        .filter_map(|(i, done)| done.is_none().then_some(i))
        .collect();
    let mut tasks = JoinSet::new();
    let mut controller = Controller::new(tuning);
    let received = telemetry.received.clone();
    let mut sample_start = Instant::now();
    let mut sample_bytes = 0u64;
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Chunks written but not yet synced; the journal only claims synced data.
    let mut unsynced: Vec<(usize, String)> = Vec::new();
    let mut last_flush = Instant::now();
    let cancel = tokio::signal::ctrl_c();
    tokio::pin!(cancel);
    while !queue.is_empty() || !tasks.is_empty() {
        refill(
            &mut tasks,
            &mut queue,
            &controller,
            client,
            info,
            state,
            tuning,
            telemetry,
        );
        tokio::select! {
            _ = &mut cancel => {
                flush(file, checkpoint_path, state, &mut unsynced)?;
                bail!("Interrupted; run 'fluxget resume <output>' to continue")
            }
            _ = ticker.tick() => {
                let seen = received.load(Ordering::Relaxed);
                let elapsed = sample_start.elapsed().as_secs_f64();
                controller.sample(seen.saturating_sub(sample_bytes) as f64 / elapsed.max(0.001));
                sample_bytes = seen;
                sample_start = Instant::now();
                telemetry.workers.store(controller.target, Ordering::Relaxed);
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                let (index, bytes, throttled) = result.context("No worker result")?? ?;
                if throttled {
                    controller.throttled();
                }
                // Start the replacement request before touching the disk.
                refill(&mut tasks, &mut queue, &controller, client, info, state, tuning, telemetry);
                file.seek(SeekFrom::Start(index as u64 * state.chunk_size))?;
                file.write_all(&bytes)?;
                unsynced.push((index, state::hash(&bytes)));
                telemetry.mark(index, ui::DONE);
                if last_flush.elapsed() >= Duration::from_secs(1) {
                    flush(file, checkpoint_path, state, &mut unsynced)?;
                    last_flush = Instant::now();
                }
            }
        }
    }
    flush(file, checkpoint_path, state, &mut unsynced)?;
    if info.etag.is_none() {
        spot_check(client, info, file, &spans(state)).await?;
    }
    Ok((resumed, controller))
}

/// Bytes compared per span when the server has no strong validator.
const SPOT_BYTES: u64 = 64 * 1024;

/// The start, middle and end of the data saved so far, as `(offset, length)` spans: the
/// start of the first and middle chunks and the end of the last one.
fn spans(state: &Checkpoint) -> Vec<(u64, u64)> {
    let done: Vec<usize> = (0..state.chunks.len())
        .filter(|&i| state.chunks[i].is_some())
        .collect();
    let (Some(&first), Some(&last)) = (done.first(), done.last()) else {
        return Vec::new();
    };
    let bounds = |i: usize| {
        let start = i as u64 * state.chunk_size;
        (start, (start + state.chunk_size).min(state.size))
    };
    let mut out = Vec::new();
    for index in [first, done[done.len() / 2]] {
        let (start, end) = bounds(index);
        out.push((start, SPOT_BYTES.min(end - start)));
    }
    let (start, end) = bounds(last);
    let len = SPOT_BYTES.min(end - start);
    out.push((end - len, len));
    out.sort_unstable();
    out.dedup();
    out
}

/// A server that sends no ETag or Last-Modified cannot say the file stayed the same, so a few
/// spans are fetched again and compared with what is on disk. It catches a file replaced
/// mid-download; it is not a substitute for `--sha256`.
async fn spot_check(
    client: &Client,
    info: &Info,
    file: &mut File,
    spans: &[(u64, u64)],
) -> Result<()> {
    let size = info.size.context("Spot check requires size")?;
    for &(start, len) in spans {
        let end = start + len - 1;
        let builder = info.guard(
            client
                .get(&info.url)
                .header("range", format!("bytes={start}-{end}")),
        );
        let (mut response, _) = http::request(builder, 2).await?;
        ensure!(
            response.status() == StatusCode::PARTIAL_CONTENT,
            "Server stopped honoring ranges during verification"
        );
        ensure!(
            http::range(
                http::header(&response, "content-range").context("Missing Content-Range")?
            )? == (start, end, size),
            "Server returned the wrong Content-Range during verification"
        );
        info.unchanged(&response)?;
        let mut remote = Vec::with_capacity(len as usize);
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                remote.len() + chunk.len() <= len as usize,
                "Verification body exceeds requested size"
            );
            remote.extend_from_slice(&chunk);
        }
        let mut local = vec![0u8; len as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut local)?;
        ensure!(
            remote == local,
            "Remote file changed while downloading (bytes {start}-{end} differ); partial data retained, delete the .fluxget.part and .fluxget.json files to restart"
        );
    }
    Ok(())
}

/// Data reaches stable storage before the journal claims it.
fn flush(
    file: &File,
    checkpoint_path: &Path,
    state: &mut Checkpoint,
    unsynced: &mut Vec<(usize, String)>,
) -> Result<()> {
    if unsynced.is_empty() {
        return Ok(());
    }
    file.sync_data()?;
    for (index, hash) in unsynced.drain(..) {
        state.chunks[index] = Some(hash);
    }
    state::save(checkpoint_path, state)
}

#[allow(clippy::too_many_arguments)]
fn refill(
    tasks: &mut JoinSet<Result<(usize, Vec<u8>, bool)>>,
    queue: &mut VecDeque<usize>,
    controller: &Controller,
    client: &Client,
    info: &Info,
    state: &Checkpoint,
    tuning: &Tuning,
    telemetry: &Arc<Telemetry>,
) {
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
            telemetry.clone(),
        ));
    }
}

async fn single(
    client: &Client,
    info: &Info,
    file: &mut File,
    tuning: &Tuning,
    telemetry: &Arc<Telemetry>,
) -> Result<u64> {
    let cancel = tokio::signal::ctrl_c();
    tokio::pin!(cancel);
    for attempt in 0..=tuning.retries {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        telemetry.received.store(0, Ordering::Relaxed);
        let builder = info.guard(client.get(&info.url));
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
        info.unchanged(&response)?;
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
                    telemetry
                        .received
                        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
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
    let lock = state::lock(&output)?;
    let part = state::sidecar(&output, ".fluxget.part");
    let checkpoint_path = state::sidecar(&output, ".fluxget.json");
    // Ranges need a verified length. Every ranged request carries the strongest precondition
    // the server offers (ETag, then Last-Modified); a server that offers neither is spot-checked.
    let use_ranges = !tuning.single_stream && info.ranges && info.size.is_some_and(|n| n > 0);
    let mut journal = if checkpoint_path.exists() {
        let loaded = state::load(&checkpoint_path)?;
        ensure!(use_ranges && loaded.url == info.url && Some(loaded.size) == info.size && loaded.etag == info.etag && loaded.weak_etag == info.weak_etag && loaded.last_modified == info.last_modified && loaded.expected_sha256 == expected,
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
    let telemetry = Telemetry::new(
        output
            .file_name()
            .map_or_else(|| "download".into(), |n| n.to_string_lossy().into_owned()),
        url::Url::parse(&info.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default(),
        info.size,
        journal.as_ref().map_or(0, |j| j.chunks.len()),
        tuning.connections as usize,
    );
    // The display removes itself when dropped, including on every error path.
    let display = (!tuning.quiet && !tuning.json).then(|| ui::Display::start(telemetry.clone()));
    if journal.is_some() {
        paused::remember(&output);
    }
    let started = Instant::now();
    let (bytes, resumed, peak, changes, strategy) = if let Some(journal) = journal.as_mut() {
        let (resumed, controller) = segmented(
            &client,
            &info,
            &mut file,
            &checkpoint_path,
            journal,
            tuning,
            &telemetry,
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
        let bytes = single(&client, &info, &mut file, tuning, &telemetry).await?;
        (bytes, 0, 1, 0, "single-stream")
    };
    file.sync_all()?;
    drop(file);
    telemetry.phase.store(ui::VERIFYING, Ordering::Relaxed);
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
    // A finished download leaves nothing beside it, which matters in a shared Downloads folder.
    drop(lock);
    let _ = fs::remove_file(state::sidecar(&output, ".fluxget.lock"));
    paused::forget(&output);
    if let Some(display) = display {
        display.finish();
    }
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
        validator: info.validator(),
        peak_connections: peak,
        concurrency_changes: changes,
    };
    if tuning.json {
        println!("{}", serde_json::to_string(&summary)?);
    } else if dialoguer::console::Term::stdout().is_term() {
        let name = summary
            .output
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let guard_note = match (strategy, summary.validator) {
            ("single-stream", _) | (_, "etag") => "",
            (_, "last-modified") => "   ·   guarded by Last-Modified",
            (_, "weak-etag") => "   ·   guarded by weak ETag, spot-checked",
            _ => "   ·   server sends no validator, spot-checked",
        };
        let resumed_note = if resumed > 0 {
            format!("   resumed {}", ui::bytes(resumed))
        } else {
            String::new()
        };
        println!(
            "\n  \x1b[32m✔\x1b[0m  \x1b[1;97mDownloaded\x1b[0m  {name}\n     \x1b[90m{}\x1b[0m\n     {} in {}   {:.1} MiB/s   up to {} connections{resumed_note}\n     \x1b[90mSHA-256 {}{guard_note}\x1b[0m\n",
            summary.output.display(),
            ui::bytes(bytes),
            ui::duration(elapsed),
            summary.mib_per_second,
            peak,
            summary.sha256
        );
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
    fn tuning(connections: u8, fixed: bool) -> Tuning {
        Tuning {
            connections,
            fixed,
            single_stream: false,
            chunk_kib: 2048,
            retries: 4,
            json: false,
            quiet: true,
        }
    }
    #[test]
    fn ramps_quickly_while_throughput_improves() {
        let mut c = Controller::new(&tuning(16, false));
        assert_eq!(c.target, 4);
        c.sample(100.0);
        assert_eq!(c.target, 6);
        c.sample(100.0); // settling sample is ignored
        assert_eq!(c.target, 6);
        c.sample(150.0);
        assert_eq!(c.target, 9);
        c.sample(150.0);
        c.sample(220.0);
        assert_eq!(c.target, 13);
        c.sample(220.0);
        c.sample(320.0);
        assert_eq!(c.target, 16);
    }
    #[test]
    fn plateau_rolls_back_then_probes_again() {
        let mut c = Controller::new(&tuning(16, false));
        c.sample(100.0);
        c.sample(100.0);
        c.sample(102.0); // 4 -> 6 gave no gain: first strike only
        assert_eq!(c.target, 6);
        c.sample(103.0);
        assert_eq!(c.target, 4);
        for _ in 0..PLATEAU_SAMPLES {
            c.sample(100.0);
            assert_eq!(c.target, 4);
        }
        c.sample(100.0);
        assert_eq!(c.target, 6);
    }
    #[test]
    fn throttle_halves_and_bounds_hold() {
        let mut c = Controller::new(&tuning(8, false));
        c.throttled();
        assert_eq!(c.target, 2);
        c.throttled();
        c.throttled();
        assert_eq!(c.target, 1);
        for n in 1..200 {
            c.sample(n as f64 * 100.0);
            assert!((1..=8).contains(&c.target));
        }
        let mut fixed = Controller::new(&tuning(8, true));
        fixed.sample(1.0);
        assert_eq!(fixed.target, 8);
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
            weak_etag: None,
            last_modified: None,
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
