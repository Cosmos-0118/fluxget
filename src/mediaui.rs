//! The live display for video and audio sites, drawn from what yt-dlp reports. It looks
//! like the display for plain files: an overall bar with throughput, one row per stream
//! (video and audio arrive separately) and a map of fragments for segmented streams.
//! yt-dlp is asked for machine-readable lines (`TEMPLATES`); everything else it prints
//! only names the step it is on.

use crate::ui::{
    bar, bytes, chunk_map, duration, map_shape, transfer_stats, wave, Glyphs, Line, Palette, State,
    DONE, IN_FLIGHT, PENDING,
};
use serde_json::Value;
use std::sync::Mutex;

/// Arguments that make yt-dlp print one parsable line per event. Every value that can hold
/// arbitrary text goes through `j`, which escapes it onto a single line.
pub const TEMPLATES: [&str; 11] = [
    "--no-quiet",
    "--newline",
    "--progress",
    "--print",
    "before_dl:FXI|%(.{title,format_id,vcodec,acodec,filesize,filesize_approx,height,fps,ext})j|%(requested_formats.:.{format_id,vcodec,acodec,filesize,filesize_approx,height,fps,ext})j",
    "--print",
    "after_move:FXD|%(filepath)j",
    "--progress-template",
    "download:FXP|%(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.fragment_index)s|%(progress.fragment_count)s|%(info.format_id)s",
    "--progress-template",
    "postprocess:FXQ|%(progress.status)s|%(progress.postprocessor)s",
];

/// Fragments yt-dlp fetches at once (`--concurrent-fragments`), drawn as in flight.
const FRAGMENTS_AT_ONCE: u64 = 16;
/// A stream whose first report is already this far along was resumed, not started.
const RESUME_THRESHOLD: u64 = 1 << 20;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Video,
    Audio,
    Both,
}

#[derive(Clone, Debug)]
pub struct Stream {
    pub id: String,
    pub kind: Kind,
    pub label: String,
    pub total: Option<u64>,
    pub done: u64,
    /// Bytes already on disk when this stream's first progress arrived.
    pub base: Option<u64>,
    pub fragments: Option<(u64, u64)>,
    pub started: bool,
    pub finished: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub title: Option<String>,
    /// What yt-dlp last said it is doing, before and between downloads.
    pub step: String,
    pub streams: Vec<Stream>,
    /// The postprocessing step in progress; once set the download itself is over.
    pub post: Option<String>,
    pub path: Option<String>,
}

/// What the display reads while the reader thread feeds it lines.
pub struct MediaTelemetry {
    pub source: String,
    inner: Mutex<Progress>,
}

fn number(text: &str) -> Option<u64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|n| *n >= 0.0)
        .map(|n| n as u64)
}

fn present(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .filter(|s| !s.is_empty() && *s != "none")
        .map(str::to_owned)
}

/// `avc1.640028` and `mp4a.40.2` read as `avc1` and `mp4a`.
fn codec(full: &str) -> &str {
    full.split('.').next().unwrap_or(full)
}

fn stream_of(format: &Value) -> Stream {
    let video = present(format, "vcodec");
    let audio = present(format, "acodec");
    let kind = match (&video, &audio) {
        (Some(_), None) => Kind::Video,
        (None, Some(_)) => Kind::Audio,
        _ => Kind::Both,
    };
    let ext = present(format, "ext");
    let mut parts: Vec<String> = Vec::new();
    if kind != Kind::Audio {
        if let Some(height) = format.get("height").and_then(Value::as_u64) {
            let fps = format.get("fps").and_then(Value::as_f64).unwrap_or(0.0);
            parts.push(if fps > 30.0 {
                format!("{height}p{}", fps.round())
            } else {
                format!("{height}p")
            });
        }
    }
    let used = if kind == Kind::Audio { &audio } else { &video };
    if let Some(name) = used {
        parts.push(codec(name).to_owned());
    }
    parts.extend(ext);
    let total = ["filesize", "filesize_approx"]
        .iter()
        .find_map(|key| format.get(*key)?.as_f64())
        .filter(|n| *n > 0.0)
        .map(|n| n as u64);
    Stream {
        id: present(format, "format_id").unwrap_or_default(),
        kind,
        label: parts.join(" · "),
        total,
        done: 0,
        base: None,
        fragments: None,
        started: false,
        finished: false,
    }
}

/// `[youtube] abc123: Downloading webpage` and `[youtube] [jsc:deno] Solving ...` say what
/// yt-dlp is doing; only the sentence is kept.
fn sentence(line: &str) -> &str {
    let mut text = line.trim();
    while text.starts_with('[') {
        let Some(end) = text.find(']') else { break };
        text = text[end + 1..].trim_start();
    }
    match text.split_once(": ") {
        Some((id, rest)) if !id.contains(' ') => rest,
        _ => text,
    }
}

/// A human name for a yt-dlp postprocessor.
fn post_step(name: &str) -> String {
    match name {
        "Merger" => "Merging video and audio".into(),
        "FFmpegExtractAudio" => "Extracting audio".into(),
        "FFmpegVideoRemuxer" | "FFmpegVideoConvertor" => "Converting the video".into(),
        "FFmpegMetadata" | "EmbedThumbnail" | "FFmpegEmbedSubtitle" => "Adding metadata".into(),
        "MoveFiles" => "Moving the file into place".into(),
        other if other.starts_with("FFmpegFixup") => "Fixing up the container".into(),
        other => format!("Processing ({other})"),
    }
}

impl Progress {
    fn stream(&mut self, id: &str) -> &mut Stream {
        let at = self
            .streams
            .iter()
            .position(|s| s.id == id)
            .unwrap_or_else(|| {
                self.streams.push(Stream {
                    id: id.to_owned(),
                    kind: Kind::Both,
                    label: String::new(),
                    total: None,
                    done: 0,
                    base: None,
                    fragments: None,
                    started: false,
                    finished: false,
                });
                self.streams.len() - 1
            });
        &mut self.streams[at]
    }

    fn info(&mut self, rest: &str) {
        let mut values = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
        let Some(Ok(item)) = values.next() else {
            return;
        };
        let after = rest[values.byte_offset()..].trim_start_matches('|');
        let merged = serde_json::from_str::<Value>(after).ok();
        self.title = present(&item, "title");
        let formats = merged
            .as_ref()
            .and_then(Value::as_array)
            .filter(|list| !list.is_empty());
        let found: Vec<Stream> = match formats {
            Some(list) => list.iter().map(stream_of).collect(),
            None => vec![stream_of(&item)],
        };
        // A second `before_dl` (another item) starts the list over.
        self.streams = found;
        self.step.clear();
    }

    fn progress(&mut self, fields: &[&str]) {
        let [status, downloaded, total, estimate, index, count, id] = fields else {
            return;
        };
        let Some(downloaded) = number(downloaded) else {
            return;
        };
        let stream = self.stream(id);
        stream.started = true;
        stream
            .base
            .get_or_insert(if downloaded >= RESUME_THRESHOLD {
                downloaded
            } else {
                0
            });
        if let Some(total) = number(total)
            .or_else(|| number(estimate))
            .filter(|n| *n > 0)
        {
            stream.total = Some(total.max(downloaded));
        }
        stream.done = downloaded;
        if let (Some(index), Some(count)) = (number(index), number(count)) {
            if count > 1 {
                stream.fragments = Some((index.min(count), count));
            }
        }
        if *status == "finished" {
            stream.finished = true;
            stream.done = stream.total.unwrap_or(downloaded).max(downloaded);
            stream.total = Some(stream.done);
        }
    }

    /// Bytes downloaded, bytes expected (when every stream's size is known) and how much of
    /// the downloaded part is this run's work and how much was already on disk.
    pub fn totals(&self) -> (u64, Option<u64>, u64, u64) {
        let done: u64 = self.streams.iter().map(|s| s.done).sum();
        let total = self
            .streams
            .iter()
            .map(|s| s.total)
            .sum::<Option<u64>>()
            .filter(|n| *n > 0);
        let resumed: u64 = self.streams.iter().map(|s| s.base.unwrap_or(0)).sum();
        (
            total.map_or(done, |t| done.min(t)),
            total,
            done.saturating_sub(resumed),
            resumed,
        )
    }
}

impl MediaTelemetry {
    pub fn new(source: String) -> Self {
        Self {
            source,
            inner: Mutex::new(Progress::default()),
        }
    }

    pub fn snapshot(&self) -> Progress {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Takes one line of yt-dlp's standard output.
    pub fn feed(&self, line: &str) {
        let line = line.trim_end();
        let mut p = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rest) = line.strip_prefix("FXI|") {
            p.info(rest);
        } else if let Some(rest) = line.strip_prefix("FXP|") {
            p.progress(&rest.split('|').collect::<Vec<_>>());
        } else if let Some(rest) = line.strip_prefix("FXQ|") {
            if let Some(("started", name)) = rest.split_once('|') {
                p.post = Some(post_step(name));
            }
        } else if let Some(rest) = line.strip_prefix("FXD|") {
            p.path = serde_json::from_str::<String>(rest).ok();
        } else if !line.is_empty()
            && !line.starts_with("[download]")
            && !line.starts_with("Deleting original")
        {
            p.step = sentence(line).to_owned();
        }
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Video => "video",
        Kind::Audio => "audio",
        Kind::Both => "media",
    }
}

fn stream_row(line: &mut Line, p: &Palette, g: &Glyphs, index: usize, s: &Stream, frame: usize) {
    let fraction = match (s.finished, s.total) {
        (true, _) => 1.0,
        (_, Some(total)) if total > 0 => s.done as f64 / total as f64,
        _ => 0.0,
    };
    line.gap(2)
        .put(p.dim, &format!("{index:>2}"))
        .gap(2)
        .put(p.white, &format!("{:<6}", kind_name(s.kind)))
        .put(p.dim, &format!("{:<22}", s.label));
    if s.started {
        bar(line, p, g, Some(fraction), 12, frame, false);
    } else {
        for _ in 0..12 {
            line.put(p.dim, &g.empty.to_string());
        }
    }
    line.gap(2);
    if s.finished {
        line.put(p.green, &format!("{:>4}", "100%"))
            .gap(2)
            .put(p.green, &g.done.to_string())
            .put(p.dim, &format!(" {}", bytes(s.done)));
    } else if s.started {
        line.put(p.white, &format!("{:>3.0}%", fraction * 100.0))
            .gap(2)
            .put(p.bright, &g.down.to_string())
            .put(p.dim, &format!(" {}", bytes(s.done)));
    } else {
        line.put(p.dim, "waiting");
    }
}

/// Draws every line of one frame; richer sections drop away first when the window is small.
pub fn render(t: &MediaTelemetry, st: &mut State, rows: usize, cols: usize) -> Vec<String> {
    let p = Palette::new();
    let g = Glyphs::new();
    let width = cols.saturating_sub(1).clamp(24, 100);
    let frame = (st.started.elapsed().as_millis() / 80) as usize;
    let snap = t.snapshot();
    let (done, total, received, resumed) = snap.totals();
    st.observe(received);
    let fraction = total.map(|n| done as f64 / n as f64);
    let finishing = snap.post.is_some();
    let waiting = snap.streams.is_empty() && !finishing;
    let fragmented = snap
        .streams
        .iter()
        .find(|s| s.started && !s.finished && s.fragments.is_some());

    let rows_streams = if snap.streams.len() > 1 {
        3 + snap.streams.len()
    } else {
        0
    };
    let rows_map = fragmented.map_or(0, |s| {
        let (_, map) = map_shape(s.fragments.map_or(0, |f| f.1) as usize, width);
        if map > 0 && !finishing {
            3 + map
        } else {
            0
        }
    });
    let budget = if rows == 0 { 24 } else { rows }.saturating_sub(2);
    let basic = 4 + 2;
    let level = if basic + rows_streams + rows_map <= budget {
        3
    } else if basic + rows_streams <= budget {
        2
    } else {
        1
    };

    let mut out: Vec<String> = Vec::new();
    let title = snap
        .title
        .clone()
        .unwrap_or_else(|| "Getting video details".to_owned());
    if level >= 2 {
        out.push(String::new());
    }
    let mut head = Line::new(width, &p);
    head.gap(2);
    wave(&mut head, &p, &g, frame, 7);
    head.gap(3).put(&format!("{}{}", p.bold, p.white), &title);
    out.push(head.text);
    if level >= 2 {
        let mut meta = Line::new(width, &p);
        meta.gap(12).put(p.dim, &t.source);
        if let Some(total) = total {
            meta.put(p.dim, &format!("   ·   {}", bytes(total)));
        }
        match snap.streams.as_slice() {
            [] => {}
            [only] if !only.label.is_empty() => {
                meta.put(p.dim, &format!("   ·   {}", only.label));
            }
            [_] => {}
            many => {
                let kinds: Vec<&str> = many.iter().map(|s| kind_name(s.kind)).collect();
                meta.put(p.dim, &format!("   ·   {}", kinds.join(" + ")));
            }
        }
        out.push(meta.text);
        out.push(String::new());
    }

    let mut main = Line::new(width, &p);
    main.gap(2);
    let cells = width.saturating_sub(2 + 3 + 7);
    let shown = if finishing { Some(1.0) } else { fraction };
    bar(&mut main, &p, &g, shown, cells, frame, !finishing);
    main.gap(3);
    match (fraction, finishing) {
        (_, true) => main.put(p.green, "100%"),
        (Some(f), _) => main.put(
            &format!("{}{}", p.bold, p.white),
            &format!("{:>5.1}%", f * 100.0),
        ),
        (None, _) => main.put(p.dim, "  ..."),
    };
    out.push(main.text);

    let mut stats = Line::new(width, &p);
    stats.gap(2);
    if let Some(step) = &snap.post {
        stats.put(p.bright, step).put(p.dim, " ...");
    } else if waiting {
        let step = if snap.step.is_empty() {
            "Starting yt-dlp"
        } else {
            &snap.step
        };
        stats.put(p.bright, step).put(p.dim, " ...");
        stats.gap(5).put(
            p.dim,
            &format!("elapsed {}", duration(st.started.elapsed().as_secs_f64())),
        );
    } else {
        transfer_stats(&mut stats, &p, &g, width, st, done, total, resumed);
    }
    out.push(stats.text);

    if level >= 2 && rows_streams > 0 {
        out.push(String::new());
        let mut head = Line::new(width, &p);
        head.gap(2).put(p.dim, "STREAMS").put(
            p.dim,
            &format!(
                "   {} of {} done",
                snap.streams.iter().filter(|s| s.finished).count(),
                snap.streams.len()
            ),
        );
        out.push(head.text);
        out.push(String::new());
        for (i, s) in snap.streams.iter().enumerate() {
            let mut line = Line::new(width, &p);
            stream_row(&mut line, &p, &g, i + 1, s, frame);
            out.push(line.text);
        }
    }

    if level >= 3 && rows_map > 0 {
        if let Some(s) = fragmented {
            let (index, count) = s.fragments.unwrap_or((0, 0));
            chunk_map(
                &mut out,
                &p,
                &g,
                width,
                frame,
                "FRAGMENT MAP",
                count as usize,
                s.total.map_or(0, |total| total.div_ceil(count.max(1))),
                |fragment| {
                    let fragment = fragment as u64;
                    if fragment < index {
                        DONE
                    } else if fragment < index + FRAGMENTS_AT_ONCE {
                        IN_FLIGHT
                    } else {
                        PENDING
                    }
                },
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    const INFO: &str = r#"FXI|{"title": "I tried | every \"champ\" (\ucc44)", "format_id": "399+251", "ext": "mp4"}|[{"format_id": "399", "vcodec": "av01.0.08M.08", "acodec": "none", "filesize": 1000, "height": 1080, "fps": 60.0, "ext": "mp4"}, {"format_id": "251", "vcodec": "none", "acodec": "opus", "filesize_approx": 500.5, "ext": "webm"}]"#;

    fn started() -> MediaTelemetry {
        let t = MediaTelemetry::new("youtube.com".into());
        t.feed("[youtube] Extracting URL: https://www.youtube.com/watch?v=x");
        t.feed("[youtube] abc: Downloading webpage");
        t.feed(INFO);
        t
    }

    #[test]
    fn reads_titles_and_streams_from_the_info_line() {
        let t = started();
        let p = t.snapshot();
        assert_eq!(p.title.as_deref(), Some("I tried | every \"champ\" (채)"));
        assert_eq!(p.streams.len(), 2);
        assert_eq!(p.streams[0].kind, Kind::Video);
        assert_eq!(p.streams[0].label, "1080p60 · av01 · mp4");
        assert_eq!(p.streams[0].total, Some(1000));
        assert_eq!(p.streams[1].kind, Kind::Audio);
        assert_eq!(p.streams[1].label, "opus · webm");
        assert_eq!(p.streams[1].total, Some(500));
        // A single, unmerged format has no list of requested formats.
        let t = MediaTelemetry::new("example.com".into());
        t.feed(r#"FXI|{"title": "Clip", "format_id": "18", "vcodec": "avc1.42", "acodec": "mp4a.40.2", "ext": "mp4"}|null"#);
        let p = t.snapshot();
        assert_eq!(p.streams.len(), 1);
        assert_eq!(p.streams[0].kind, Kind::Both);
        assert_eq!(p.streams[0].label, "avc1 · mp4");
    }

    #[test]
    fn progress_accumulates_across_streams() {
        let t = started();
        t.feed("FXP|downloading|400|1000|NA|NA|NA|399");
        assert_eq!(t.snapshot().totals(), (400, Some(1500), 400, 0));
        t.feed("FXP|finished|1000|1000|NA|NA|NA|399");
        t.feed("FXP|downloading|100|500|NA|NA|NA|251");
        assert_eq!(t.snapshot().totals(), (1100, Some(1500), 1100, 0));
        t.feed("FXP|finished|500|500|NA|NA|NA|251");
        let p = t.snapshot();
        assert!(p.streams.iter().all(|s| s.finished));
        assert_eq!(p.totals(), (1500, Some(1500), 1500, 0));
    }

    #[test]
    fn a_stream_that_was_partly_downloaded_counts_as_resumed() {
        let t = started();
        t.feed("FXP|downloading|3145728|3145728|NA|NA|NA|399");
        let (done, _, received, resumed) = t.snapshot().totals();
        assert_eq!((done, received, resumed), (3145728, 0, 3145728));
        t.feed("FXP|downloading|3146728|3146728|NA|NA|NA|399");
        assert_eq!(t.snapshot().totals().2, 1000);
    }

    #[test]
    fn unknown_sizes_leave_the_total_open() {
        let t = MediaTelemetry::new("example.com".into());
        t.feed("FXP|downloading|5000|NA|NA|3|90|hls-1");
        let p = t.snapshot();
        assert_eq!(p.totals().1, None);
        assert_eq!(p.streams[0].fragments, Some((3, 90)));
        t.feed("FXP|downloading|6000|NA|20000.7|4|90|hls-1");
        assert_eq!(t.snapshot().totals().1, Some(20000));
    }

    #[test]
    fn steps_phases_and_the_result_are_tracked() {
        let t = MediaTelemetry::new("youtube.com".into());
        t.feed("[youtube] [jsc:deno] Solving JS challenges using deno");
        assert_eq!(t.snapshot().step, "Solving JS challenges using deno");
        t.feed("[youtube] abc: Downloading webpage");
        assert_eq!(t.snapshot().step, "Downloading webpage");
        t.feed("[download] Destination: C:\\x");
        assert_eq!(t.snapshot().step, "Downloading webpage");
        t.feed("FXQ|started|Merger");
        assert_eq!(
            t.snapshot().post.as_deref(),
            Some("Merging video and audio")
        );
        t.feed(r#"FXD|"C:\\Users\\x\\clip.mkv""#);
        assert_eq!(t.snapshot().path.as_deref(), Some("C:\\Users\\x\\clip.mkv"));
    }

    #[test]
    fn frames_fit_every_terminal_size_and_show_each_stage() {
        let t = MediaTelemetry::new("youtube.com".into());
        let mut state = State::new();
        let text = render(&t, &mut state, 40, 100)
            .iter()
            .map(|l| strip(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Getting video details") && text.contains("Starting yt-dlp"),
            "{text}"
        );
        t.feed(INFO);
        t.feed("FXP|downloading|600|1000|NA|7|90|399");
        for rows in [60, 30, 12, 8, 4] {
            for cols in [30, 60, 80, 120, 200] {
                let mut state = State::new();
                for line in render(&t, &mut state, rows, cols) {
                    let visible = strip(&line).chars().count();
                    assert!(visible < cols.max(25), "{visible} wide at {cols}");
                }
            }
        }
        let mut state = State::new();
        let text = render(&t, &mut state, 60, 100)
            .iter()
            .map(|l| strip(l))
            .collect::<Vec<_>>()
            .join("\n");
        for want in [
            "I tried | every",
            "STREAMS",
            "0 of 2 done",
            "FRAGMENT MAP",
            "1080p60",
            "waiting",
            "40.0%",
        ] {
            assert!(text.contains(want), "{want} missing in\n{text}");
        }
        t.feed("FXQ|started|Merger");
        let mut state = State::new();
        let text = render(&t, &mut state, 60, 100)
            .iter()
            .map(|l| strip(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Merging video and audio") && text.contains("100%"),
            "{text}"
        );
        assert!(!text.contains("FRAGMENT MAP"), "{text}");
    }
}
