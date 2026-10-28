//! The live download display: a header, an overall bar with throughput, one lane per
//! connection and a map of every chunk. This file only draws; the engine reports what
//! is happening through `Telemetry`.

use dialoguer::{console::Term, theme::ColorfulTheme};
use std::{
    collections::VecDeque,
    io::Write,
    sync::{
        atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering::Relaxed},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

pub const PENDING: u8 = 0;
pub const IN_FLIGHT: u8 = 1;
pub const DONE: u8 = 2;
pub const DOWNLOADING: u8 = 0;
pub const VERIFYING: u8 = 1;

/// Most connection lanes drawn; more connections still download, they just share a footer.
const MAX_LANES: usize = 16;
/// Most rows the chunk map may take; larger downloads share a tile between chunks.
const MAP_ROWS: usize = 8;

/// One connection working on one chunk.
pub struct Lane {
    pub slot: usize,
    pub chunk: usize,
    pub total: u64,
    pub done: AtomicU64,
}

/// What the engine reports and the display reads. All fields are cheap atomics so the
/// download path never waits for drawing.
pub struct Telemetry {
    pub name: String,
    pub source: String,
    pub total: Option<u64>,
    pub resumed: AtomicU64,
    /// Bytes received this run, including chunks still in flight.
    pub received: Arc<AtomicU64>,
    pub states: Vec<AtomicU8>,
    pub lanes: Mutex<Vec<Arc<Lane>>>,
    pub workers: AtomicUsize,
    pub max_workers: usize,
    pub phase: AtomicU8,
}

impl Telemetry {
    pub fn new(
        name: String,
        source: String,
        total: Option<u64>,
        chunks: usize,
        max_workers: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            source,
            total,
            resumed: AtomicU64::new(0),
            received: Arc::new(AtomicU64::new(0)),
            states: (0..chunks).map(|_| AtomicU8::new(PENDING)).collect(),
            lanes: Mutex::new(Vec::new()),
            workers: AtomicUsize::new(max_workers),
            max_workers,
            phase: AtomicU8::new(DOWNLOADING),
        })
    }

    pub fn mark(&self, chunk: usize, state: u8) {
        if let Some(cell) = self.states.get(chunk) {
            cell.store(state, Relaxed);
        }
    }

    /// Takes the lowest free connection slot for `chunk` until the guard drops.
    pub fn begin_lane(self: &Arc<Self>, chunk: usize, total: u64) -> LaneGuard {
        let mut lanes = self.lanes.lock().unwrap_or_else(|e| e.into_inner());
        let slot = (1..)
            .find(|slot| lanes.iter().all(|lane| lane.slot != *slot))
            .unwrap_or(1);
        let lane = Arc::new(Lane {
            slot,
            chunk,
            total,
            done: AtomicU64::new(0),
        });
        lanes.push(lane.clone());
        self.mark(chunk, IN_FLIGHT);
        LaneGuard {
            telemetry: self.clone(),
            lane,
        }
    }
}

pub struct LaneGuard {
    telemetry: Arc<Telemetry>,
    pub lane: Arc<Lane>,
}

impl Drop for LaneGuard {
    fn drop(&mut self) {
        let mut lanes = self
            .telemetry
            .lanes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        lanes.retain(|lane| lane.slot != self.lane.slot);
        // A lane that ended without its chunk being stored goes back to waiting.
        if self
            .telemetry
            .states
            .get(self.lane.chunk)
            .map(|s| s.load(Relaxed))
            == Some(IN_FLIGHT)
        {
            self.telemetry.mark(self.lane.chunk, PENDING);
        }
    }
}

/// The dialoguer look shared by every prompt: indented, with room to breathe.
pub fn theme() -> ColorfulTheme {
    use dialoguer::console::style;
    ColorfulTheme {
        prompt_prefix: style("  ?".to_string()).yellow().bright(),
        success_prefix: style("  ✔".to_string()).green(),
        error_prefix: style("  ✘".to_string()).red(),
        active_item_prefix: style("  ❯".to_string()).cyan().bright(),
        inactive_item_prefix: style("   ".to_string()),
        ..ColorfulTheme::default()
    }
}

pub(crate) struct Palette {
    pub(crate) dim: &'static str,
    pub(crate) cyan: &'static str,
    pub(crate) bright: &'static str,
    pub(crate) white: &'static str,
    pub(crate) green: &'static str,
    pub(crate) bold: &'static str,
    pub(crate) reset: &'static str,
}

impl Palette {
    pub(crate) fn new() -> Self {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            Self {
                dim: "",
                cyan: "",
                bright: "",
                white: "",
                green: "",
                bold: "",
                reset: "",
            }
        } else {
            Self {
                dim: "\x1b[90m",
                cyan: "\x1b[36m",
                bright: "\x1b[96m",
                white: "\x1b[97m",
                green: "\x1b[32m",
                bold: "\x1b[1m",
                reset: "\x1b[0m",
            }
        }
    }
}

pub(crate) struct Glyphs {
    pub(crate) full: char,
    pub(crate) empty: char,
    /// Left-aligned partial blocks, one eighth to seven eighths.
    pub(crate) partial: [char; 7],
    pub(crate) wave: [char; 8],
    pub(crate) done: char,
    pub(crate) flight: [char; 2],
    pub(crate) some: char,
    pub(crate) wait: char,
    pub(crate) down: char,
}

impl Glyphs {
    pub(crate) fn new() -> Self {
        if std::env::var_os("FLUXGET_ASCII").is_some_and(|v| !v.is_empty()) {
            Self {
                full: '#',
                empty: '-',
                partial: ['=', '=', '=', '=', '=', '=', '='],
                wave: ['_', '.', '-', '~', '=', '+', '*', '#'],
                done: '#',
                flight: ['>', '>'],
                some: '+',
                wait: '.',
                down: 'v',
            }
        } else {
            Self {
                full: '█',
                empty: '░',
                partial: ['▏', '▎', '▍', '▌', '▋', '▊', '▉'],
                wave: ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'],
                done: '■',
                flight: ['■', '▪'],
                some: '▪',
                wait: '·',
                down: '↓',
            }
        }
    }
}

/// One period of a sine wave, scaled to the eight block heights.
const WAVE: [usize; 16] = [4, 5, 6, 7, 7, 7, 6, 5, 4, 2, 1, 0, 0, 0, 1, 2];

/// A line of styled text that never grows past the terminal width.
pub(crate) struct Line {
    pub(crate) text: String,
    pub(crate) width: usize,
    max: usize,
    reset: &'static str,
}

impl Line {
    pub(crate) fn new(max: usize, palette: &Palette) -> Self {
        Self {
            text: String::new(),
            width: 0,
            max,
            reset: palette.reset,
        }
    }
    pub(crate) fn put(&mut self, style: &str, text: &str) -> &mut Self {
        let room = self.max.saturating_sub(self.width);
        let shown: String = text.chars().take(room).collect();
        if !shown.is_empty() {
            self.width += shown.chars().count();
            self.text.push_str(style);
            self.text.push_str(&shown);
            if !style.is_empty() {
                self.text.push_str(self.reset);
            }
        }
        self
    }
    pub(crate) fn gap(&mut self, n: usize) -> &mut Self {
        self.put("", &" ".repeat(n))
    }
}

pub fn bytes(n: u64) -> String {
    let n = n as f64;
    const KIB: f64 = 1024.0;
    if n >= KIB * KIB * KIB {
        format!("{:.2} GiB", n / (KIB * KIB * KIB))
    } else if n >= KIB * KIB {
        format!("{:.1} MiB", n / (KIB * KIB))
    } else if n >= KIB {
        format!("{:.0} KiB", n / KIB)
    } else {
        format!("{n:.0} B")
    }
}

pub fn duration(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Time left, from throughput over a sliding window rather than the instantaneous speed
/// the readout shows. Connection ramp-up, a slow chunk or a stall move the instant speed by
/// an order of magnitude, which would make the estimate swing the same way.
pub(crate) struct Eta {
    samples: VecDeque<(Instant, u64)>,
    rate: f64,
    updated: Instant,
    progressed: Instant,
}

/// Throughput is measured over this long, and the estimate settles over about as long.
const ETA_WINDOW: Duration = Duration::from_secs(30);
/// No estimate before this much history: the first seconds only measure ramp-up.
const ETA_WARMUP: Duration = Duration::from_secs(3);
/// No new bytes for this long counts as a stall, not a slow transfer.
const ETA_STALL: Duration = Duration::from_secs(6);

#[derive(Debug, PartialEq)]
pub(crate) enum Remaining {
    Estimating,
    Stalled,
    Seconds(f64),
}

impl Eta {
    fn new(now: Instant) -> Self {
        Self {
            samples: VecDeque::new(),
            rate: 0.0,
            updated: now,
            progressed: now,
        }
    }

    fn observe(&mut self, now: Instant, received: u64) {
        if let Some(&(at, bytes)) = self.samples.back() {
            if now.duration_since(at) < Duration::from_millis(250) {
                return;
            }
            if received > bytes {
                self.progressed = now;
            }
        } else {
            self.progressed = now;
        }
        self.samples.push_back((now, received));
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > ETA_WINDOW)
        {
            self.samples.pop_front();
        }
        let (first_at, first_bytes) = self.samples[0];
        let span = now.duration_since(first_at);
        if span < ETA_WARMUP {
            return;
        }
        let window = received.saturating_sub(first_bytes) as f64 / span.as_secs_f64();
        let dt = now.duration_since(self.updated).as_secs_f64();
        self.rate = if self.rate == 0.0 {
            window
        } else {
            // Time-based weight, so the result does not depend on how often this runs.
            let keep = (-dt / (ETA_WINDOW.as_secs_f64() / 3.0)).exp();
            keep * self.rate + (1.0 - keep) * window
        };
        self.updated = now;
    }

    pub(crate) fn remaining(&self, now: Instant, left: u64) -> Remaining {
        if left == 0 {
            return Remaining::Seconds(0.0);
        }
        if self.rate == 0.0 {
            return if now.duration_since(self.progressed) > ETA_STALL && self.samples.len() > 1 {
                Remaining::Stalled
            } else {
                Remaining::Estimating
            };
        }
        if now.duration_since(self.progressed) > ETA_STALL {
            return Remaining::Stalled;
        }
        Remaining::Seconds(left as f64 / self.rate)
    }
}

/// Coarser as the figure grows, so a long estimate does not flicker every second.
pub(crate) fn eta_text(seconds: f64) -> String {
    let s = seconds.max(0.0).round();
    duration(match s as u64 {
        0..=119 => s,
        120..=3599 => (s / 5.0).round() * 5.0,
        _ => (s / 60.0).round() * 60.0,
    })
}

pub(crate) struct State {
    pub(crate) started: Instant,
    sampled: Instant,
    sampled_bytes: u64,
    pub(crate) speed: f64,
    pub(crate) history: VecDeque<f64>,
    history_at: Instant,
    pub(crate) eta: Eta,
}

impl State {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            sampled: now,
            sampled_bytes: 0,
            speed: 0.0,
            history: VecDeque::new(),
            history_at: now,
            eta: Eta::new(now),
        }
    }
    pub(crate) fn observe(&mut self, received: u64) {
        let now = Instant::now();
        self.eta.observe(now, received);
        let span = now.duration_since(self.sampled).as_secs_f64();
        if span >= 0.25 {
            let instant = received.saturating_sub(self.sampled_bytes) as f64 / span;
            self.speed = if self.speed == 0.0 {
                instant
            } else {
                0.7 * self.speed + 0.3 * instant
            };
            self.sampled = now;
            self.sampled_bytes = received;
        }
        if now.duration_since(self.history_at) >= Duration::from_millis(500) {
            self.history_at = now;
            self.history.push_back(self.speed);
            while self.history.len() > 18 {
                self.history.pop_front();
            }
        }
    }
}

pub(crate) fn wave(line: &mut Line, p: &Palette, g: &Glyphs, frame: usize, cells: usize) {
    for i in 0..cells {
        let height = WAVE[(frame + i * 2) % WAVE.len()];
        let style = if height >= 6 {
            p.white
        } else if height >= 3 {
            p.cyan
        } else {
            p.dim
        };
        line.put(style, &g.wave[height].to_string());
    }
}

/// A bar of `cells` cells. Known progress fills smoothly with a moving glint; unknown
/// progress shows a comet bouncing along the track.
pub(crate) fn bar(
    line: &mut Line,
    p: &Palette,
    g: &Glyphs,
    fraction: Option<f64>,
    cells: usize,
    frame: usize,
    glint: bool,
) {
    if cells == 0 {
        return;
    }
    match fraction {
        Some(fraction) => {
            let exact = fraction.clamp(0.0, 1.0) * cells as f64;
            let whole = exact.floor() as usize;
            let part = ((exact - whole as f64) * 8.0).floor() as usize;
            let sweep = if glint {
                (frame % (whole + 8)) as isize - 4
            } else {
                -100
            };
            for i in 0..cells {
                if i < whole {
                    let near = (i as isize - sweep).abs() <= 1;
                    line.put(if near { p.white } else { p.cyan }, &g.full.to_string());
                } else if i == whole && part > 0 {
                    line.put(p.bright, &g.partial[part - 1].to_string());
                } else {
                    line.put(p.dim, &g.empty.to_string());
                }
            }
        }
        None => {
            let period = (2 * cells.saturating_sub(1)).max(1);
            let at = frame % period;
            let head = if at < cells { at } else { period - at };
            for i in 0..cells {
                let distance = i.abs_diff(head);
                let (style, glyph) = match distance {
                    0 => (p.white, g.full),
                    1 | 2 => (p.cyan, g.full),
                    _ => (p.dim, g.empty),
                };
                line.put(style, &glyph.to_string());
            }
        }
    }
}

pub(crate) fn sparkline(history: &VecDeque<f64>, g: &Glyphs) -> String {
    let peak = history.iter().copied().fold(0.0, f64::max);
    if peak <= 0.0 {
        return String::new();
    }
    history
        .iter()
        .map(|v| g.wave[((v / peak) * 7.0).round().clamp(0.0, 7.0) as usize])
        .collect()
}

/// Draws every line of one frame. `rows`/`cols` are the terminal size; richer sections
/// drop away first when the window is small.
fn render(t: &Telemetry, st: &mut State, rows: usize, cols: usize) -> Vec<String> {
    let p = Palette::new();
    let g = Glyphs::new();
    let width = cols.saturating_sub(1).clamp(24, 100);
    let frame = (st.started.elapsed().as_millis() / 80) as usize;
    let received = t.received.load(Relaxed);
    st.observe(received);
    let resumed = t.resumed.load(Relaxed);
    let done_bytes = match t.total {
        Some(total) => (resumed + received).min(total),
        None => resumed + received,
    };
    let fraction = t
        .total
        .filter(|n| *n > 0)
        .map(|n| done_bytes as f64 / n as f64);
    let verifying = t.phase.load(Relaxed) == VERIFYING;
    let lanes: Vec<Arc<Lane>> = t.lanes.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let chunks = t.states.len();

    let slots = t
        .workers
        .load(Relaxed)
        .max(lanes.iter().map(|l| l.slot).max().unwrap_or(0))
        .min(t.max_workers.max(1))
        .min(MAX_LANES);
    let columns = if slots > 6 && width >= 70 { 2 } else { 1 };
    let lane_rows = slots.div_ceil(columns);
    // One tile per chunk where it fits: a tile and its gap are two columns wide, about as
    // wide as a terminal row is tall, so the map reads as a square grid that fills up.
    let (_, map_rows) = map_shape(chunks, width);
    let budget = if rows == 0 { 24 } else { rows }.saturating_sub(2);
    let basic = 1 + 2 + 1 + 2;
    let with_map = basic
        + if map_rows > 0 {
            1 + 1 + 1 + map_rows
        } else {
            0
        };
    let with_lanes = with_map
        + if slots > 0 && chunks > 0 {
            1 + 1 + 1 + lane_rows
        } else {
            0
        };
    let level = if with_lanes <= budget && !verifying {
        3
    } else if with_map <= budget && !verifying {
        2
    } else {
        1
    };

    let mut out: Vec<String> = Vec::new();
    let blank = || String::new();

    // Header: animated wave, file name, then where it comes from.
    if level >= 2 || verifying {
        out.push(blank());
    }
    let mut head = Line::new(width, &p);
    head.gap(2);
    wave(&mut head, &p, &g, frame, 7);
    head.gap(3).put(&format!("{}{}", p.bold, p.white), &t.name);
    out.push(head.text);
    if level >= 2 || verifying {
        let live = lanes.len();
        let mut meta = Line::new(width, &p);
        meta.gap(12).put(p.dim, &t.source);
        if let Some(total) = t.total {
            meta.put(p.dim, &format!("   ·   {}", bytes(total)));
        }
        if verifying {
            meta.put(p.dim, "   ·   download complete");
        } else if chunks > 0 {
            meta.put(
                p.dim,
                &format!("   ·   {live} of {} connections", slots.max(live)),
            );
        } else {
            meta.put(p.dim, "   ·   1 connection");
        }
        out.push(meta.text);
        out.push(blank());
    }

    // Overall bar.
    let mut main = Line::new(width, &p);
    main.gap(2);
    let cells = width.saturating_sub(2 + 3 + 7);
    bar(
        &mut main,
        &p,
        &g,
        if verifying { Some(1.0) } else { fraction },
        cells,
        frame,
        !verifying,
    );
    main.gap(3);
    match (fraction, verifying) {
        (_, true) => main.put(p.green, "100%"),
        (Some(f), _) => main.put(
            &format!("{}{}", p.bold, p.white),
            &format!("{:>5.1}%", f * 100.0),
        ),
        (None, _) => main.put(p.dim, "  ..."),
    };
    out.push(main.text);

    // Numbers.
    let mut stats = Line::new(width, &p);
    stats.gap(2);
    if verifying {
        stats.put(p.bright, "Verifying SHA-256").put(p.dim, " ...");
    } else {
        transfer_stats(&mut stats, &p, &g, width, st, done_bytes, t.total, resumed);
    }
    out.push(stats.text);

    if level >= 3 {
        out.push(blank());
        let mut title = Line::new(width, &p);
        title
            .gap(2)
            .put(p.dim, "CONNECTIONS")
            .put(p.dim, &format!("   {} active", lanes.len()));
        out.push(title.text);
        out.push(blank());
        let column_width = (width - 2) / columns;
        for row in 0..lane_rows {
            let mut line = Line::new(width, &p);
            line.gap(2);
            for column in 0..columns {
                let slot = row + column * lane_rows + 1;
                if slot > slots {
                    break;
                }
                let start = line.width;
                lane_cell(
                    &mut line,
                    &p,
                    &g,
                    lanes.iter().find(|l| l.slot == slot),
                    slot,
                    frame,
                );
                let used = line.width - start;
                if column + 1 < columns {
                    line.gap(column_width.saturating_sub(used));
                }
            }
            out.push(line.text);
        }
    }

    if level >= 2 && map_rows > 0 {
        chunk_map(
            &mut out,
            &p,
            &g,
            width,
            frame,
            "CHUNK MAP",
            chunks,
            chunk_bytes(t),
            |chunk| t.states[chunk].load(Relaxed),
        );
    }
    out
}

/// The numbers under the bar: size so far, speed with its sparkline, time left, time spent.
#[allow(clippy::too_many_arguments)]
pub(crate) fn transfer_stats(
    stats: &mut Line,
    p: &Palette,
    g: &Glyphs,
    width: usize,
    st: &State,
    done: u64,
    total: Option<u64>,
    resumed: u64,
) {
    match total {
        Some(total) => stats
            .put(p.white, &bytes(done))
            .put(p.dim, &format!(" of {}", bytes(total))),
        None => stats.put(p.white, &bytes(done)),
    };
    stats.gap(5).put(
        p.bright,
        &format!("{} {}/s", g.down, bytes(st.speed as u64)),
    );
    let spark = sparkline(&st.history, g);
    if !spark.is_empty() && width >= 64 {
        stats.gap(2).put(p.cyan, &spark);
    }
    if let Some(total) = total {
        let left = total.saturating_sub(done);
        stats.gap(5);
        match st.eta.remaining(Instant::now(), left) {
            Remaining::Seconds(s) => stats.put(p.white, &eta_text(s)).put(p.dim, " left"),
            Remaining::Estimating => stats.put(p.dim, "estimating..."),
            Remaining::Stalled => stats.put(p.dim, "stalled"),
        };
    }
    stats.gap(5).put(
        p.dim,
        &format!("elapsed {}", duration(st.started.elapsed().as_secs_f64())),
    );
    if resumed > 0 && width >= 90 {
        stats
            .gap(5)
            .put(p.dim, &format!("resumed {}", bytes(resumed)));
    }
}

/// Rows the chunk map takes for `chunks` tiles at this width, as `(per_cell, rows)`; zero rows
/// when there is nothing to draw.
pub(crate) fn map_shape(chunks: usize, width: usize) -> (usize, usize) {
    let columns = (width - 4) / 2;
    let per_cell = chunks.div_ceil(columns * MAP_ROWS).max(1);
    let cells = chunks.div_ceil(per_cell);
    (per_cell, cells.div_ceil(columns))
}

/// The map of every chunk: a title with its legend, then one tile per chunk (or per group of
/// chunks on large downloads) that fills up as they finish. `state` reports `PENDING`,
/// `IN_FLIGHT` or `DONE` for one chunk.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chunk_map(
    out: &mut Vec<String>,
    p: &Palette,
    g: &Glyphs,
    width: usize,
    frame: usize,
    name: &str,
    chunks: usize,
    chunk_bytes: u64,
    state: impl Fn(usize) -> u8,
) {
    let (per_cell, rows) = map_shape(chunks, width);
    if rows == 0 {
        return;
    }
    let columns = (width - 4) / 2;
    let cells = chunks.div_ceil(per_cell);
    out.push(String::new());
    let mut title = Line::new(width, p);
    title.gap(2).put(p.dim, name);
    if chunk_bytes > 0 {
        title.put(
            p.dim,
            &format!("   each cell = {}   ", bytes(per_cell as u64 * chunk_bytes)),
        );
    } else {
        title.gap(3);
    }
    title
        .put(p.cyan, &g.done.to_string())
        .put(p.dim, " done   ");
    title
        .put(p.white, &g.flight[0].to_string())
        .put(p.dim, " downloading   ");
    title.put(p.dim, &format!("{} waiting", g.wait));
    out.push(title.text);
    out.push(String::new());
    for row in 0..rows {
        let mut line = Line::new(width, p);
        line.gap(2);
        for cell in (row * columns)..((row + 1) * columns).min(cells) {
            let from = cell * per_cell;
            let to = ((cell + 1) * per_cell).min(chunks);
            let (mut done, mut flying) = (0, 0);
            for chunk in from..to {
                match state(chunk) {
                    DONE => done += 1,
                    IN_FLIGHT => flying += 1,
                    _ => {}
                }
            }
            if flying > 0 {
                line.put(p.white, &g.flight[(frame / 2 + cell) % 2].to_string());
            } else if done == to - from {
                line.put(p.cyan, &g.done.to_string());
            } else if done > 0 {
                line.put(p.cyan, &g.some.to_string());
            } else {
                line.put(p.dim, &g.wait.to_string());
            }
            line.gap(1);
        }
        out.push(line.text);
    }
}

/// Bytes covered by one chunk, from the connection data when the map is drawn.
fn chunk_bytes(t: &Telemetry) -> u64 {
    match (t.total, t.states.len()) {
        (Some(total), n) if n > 0 => total.div_ceil(n as u64),
        _ => 0,
    }
}

fn lane_cell(
    line: &mut Line,
    p: &Palette,
    g: &Glyphs,
    lane: Option<&Arc<Lane>>,
    slot: usize,
    frame: usize,
) {
    line.put(p.dim, &format!("{slot:>2}")).gap(2);
    match lane {
        Some(lane) => {
            let fraction = if lane.total == 0 {
                0.0
            } else {
                lane.done.load(Relaxed) as f64 / lane.total as f64
            };
            bar(line, p, g, Some(fraction), 12, frame, false);
            line.gap(2)
                .put(p.white, &format!("{:>3.0}%", fraction * 100.0))
                .gap(2)
                .put(p.dim, &format!("#{:<5}", lane.chunk + 1));
        }
        None => {
            for _ in 0..12 {
                line.put(p.dim, &g.empty.to_string());
            }
            line.gap(2).put(p.dim, "idle");
            line.gap(9);
        }
    }
}

#[cfg(windows)]
fn enable_ansi() -> bool {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_ERROR_HANDLE,
    };
    // SAFETY: plain Win32 calls on the process's own standard error handle.
    unsafe {
        let handle = GetStdHandle(STD_ERROR_HANDLE);
        let mut mode = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
            || SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn enable_ansi() -> bool {
    true
}

fn write_err(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}

/// Whether the live display can be drawn: stderr is a terminal that understands escapes.
pub fn interactive() -> bool {
    Term::stderr().is_term() && enable_ansi()
}

/// Owns the drawing task. Dropping it, even on an error path, removes the display and
/// brings the cursor back, so messages printed afterwards start on a clean line.
pub struct Display {
    task: Option<JoinHandle<()>>,
    drawn: Arc<AtomicUsize>,
}

impl Display {
    /// Starts drawing when stderr is an interactive terminal; otherwise does nothing.
    pub fn start(telemetry: Arc<Telemetry>) -> Self {
        let mut state = State::new();
        Self::spawn(move |rows, cols| render(&telemetry, &mut state, rows, cols))
    }

    /// Calls `draw(rows, cols)` for a new frame every 80 ms until dropped, if the terminal
    /// can show it.
    pub fn spawn(mut draw: impl FnMut(usize, usize) -> Vec<String> + Send + 'static) -> Self {
        let drawn = Arc::new(AtomicUsize::new(0));
        if !interactive() {
            return Self { task: None, drawn };
        }
        let lines = drawn.clone();
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(80));
            loop {
                tick.tick().await;
                let (rows, cols) = Term::stderr().size();
                paint(&draw(rows as usize, cols as usize), &lines);
            }
        });
        Self {
            task: Some(task),
            drawn,
        }
    }

    pub fn finish(self) {}
}

fn paint(frame: &[String], drawn: &AtomicUsize) {
    let before = drawn.load(Relaxed);
    let mut out = String::from("\x1b[?25l");
    if before > 0 {
        out.push_str(&format!("\x1b[{before}A"));
    }
    for line in frame {
        out.push_str("\r\x1b[2K");
        out.push_str(line);
        out.push('\n');
    }
    if frame.len() < before {
        out.push_str("\x1b[J");
    }
    drawn.store(frame.len(), Relaxed);
    write_err(&out);
}

impl Drop for Display {
    fn drop(&mut self) {
        let Some(task) = self.task.take() else {
            return;
        };
        task.abort();
        let before = self.drawn.swap(0, Relaxed);
        let mut out = String::new();
        if before > 0 {
            out.push_str(&format!("\x1b[{before}A"));
        }
        out.push_str("\r\x1b[J\x1b[?25h");
        write_err(&out);
    }
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

    fn sample(chunks: usize, workers: usize) -> Arc<Telemetry> {
        let t = Telemetry::new(
            "LucidFrame-2.0.0-win-x64.exe".into(),
            "github.com".into(),
            Some(chunks as u64 * 2 * 1024 * 1024),
            chunks,
            workers,
        );
        for i in 0..chunks / 2 {
            t.mark(i, DONE);
        }
        t.received.store(chunks as u64 * 1024 * 1024, Relaxed);
        t
    }

    #[test]
    fn frames_fit_every_terminal_size_and_degrade_with_height() {
        let t = sample(154, 16);
        let guards: Vec<_> = (0..9)
            .map(|i| t.begin_lane(80 + i, 2 * 1024 * 1024))
            .collect();
        guards[0].lane.done.store(1024 * 1024, Relaxed);
        let mut previous = usize::MAX;
        for rows in [60, 30, 22, 14, 8, 4] {
            for cols in [30, 60, 80, 120, 200] {
                let mut state = State::new();
                let lines = render(&t, &mut state, rows, cols);
                assert!(!lines.is_empty());
                for line in &lines {
                    let visible = strip(line).chars().count();
                    assert!(
                        visible < cols.max(25),
                        "{visible} wide at {cols}: {}",
                        strip(line)
                    );
                }
            }
            let mut state = State::new();
            let height = render(&t, &mut state, rows, 100).len();
            assert!(height <= previous, "taller at {rows} rows");
            previous = height;
        }
        let mut state = State::new();
        let full = render(&t, &mut state, 60, 100)
            .iter()
            .map(|l| strip(l))
            .collect::<Vec<_>>();
        let text = full.join("\n");
        assert!(
            text.contains("CONNECTIONS") && text.contains("CHUNK MAP"),
            "{text}"
        );
        assert!(
            text.contains("LucidFrame-2.0.0-win-x64.exe") && text.contains("50.0%"),
            "{text}"
        );
        assert!(text.contains("9 of 16 connections"), "{text}");
    }

    fn feed(eta: &mut Eta, start: Instant, from: u64, to: u64, rate: u64) -> u64 {
        let mut bytes = 0;
        for tenth in from * 10..=to * 10 {
            bytes = tenth * rate / 10;
            eta.observe(start + Duration::from_millis(tenth * 100), bytes);
        }
        bytes
    }

    #[test]
    fn eta_waits_for_history_then_follows_steady_throughput() {
        let start = Instant::now();
        let mut eta = Eta::new(start);
        let mib = 1 << 20;
        let total = 3000 * mib;
        feed(&mut eta, start, 0, 2, 10 * mib);
        assert_eq!(
            eta.remaining(start + Duration::from_secs(2), total),
            Remaining::Estimating
        );
        let done = feed(&mut eta, start, 3, 20, 10 * mib);
        let now = start + Duration::from_secs(20);
        let Remaining::Seconds(s) = eta.remaining(now, total - done) else {
            panic!("expected an estimate");
        };
        assert!((s - 280.0).abs() < 3.0, "{s}");
    }

    #[test]
    fn eta_ignores_a_brief_burst_or_dip() {
        let start = Instant::now();
        let mut eta = Eta::new(start);
        let mib = 1 << 20;
        let mut bytes = feed(&mut eta, start, 0, 30, 10 * mib);
        let before = eta.rate;
        // Two seconds at ten times the speed, then two seconds of nothing.
        for tenth in 301..=320 {
            bytes += 10 * mib;
            eta.observe(start + Duration::from_millis(tenth * 100), bytes);
        }
        let after_burst = eta.rate;
        assert!(after_burst < before * 1.4, "{before} -> {after_burst}");
        for tenth in 321..=340 {
            eta.observe(start + Duration::from_millis(tenth * 100), bytes);
        }
        assert!(eta.rate > before * 0.7, "{before} -> {}", eta.rate);
    }

    #[test]
    fn eta_reports_a_stall_instead_of_a_huge_number() {
        let start = Instant::now();
        let mut eta = Eta::new(start);
        let mib = 1 << 20;
        let bytes = feed(&mut eta, start, 0, 10, 5 * mib);
        for tenth in 101..=200 {
            eta.observe(start + Duration::from_millis(tenth * 100), bytes);
        }
        assert_eq!(
            eta.remaining(start + Duration::from_secs(20), 100 * mib),
            Remaining::Stalled
        );
        assert_eq!(
            eta.remaining(start + Duration::from_secs(20), 0),
            Remaining::Seconds(0.0)
        );
    }

    #[test]
    fn long_estimates_are_rounded_so_they_do_not_flicker() {
        assert_eq!(eta_text(59.6), "1m 00s");
        assert_eq!(eta_text(301.0), "5m 00s");
        assert_eq!(eta_text(304.0), "5m 05s");
        assert_eq!(eta_text(3850.0), "1h 04m");
    }

    #[test]
    fn a_lane_that_dies_returns_its_chunk_to_waiting() {
        let t = sample(4, 2);
        let guard = t.begin_lane(3, 100);
        assert_eq!(t.states[3].load(Relaxed), IN_FLIGHT);
        drop(guard);
        assert_eq!(t.states[3].load(Relaxed), PENDING);
        let guard = t.begin_lane(1, 100);
        t.mark(1, DONE);
        drop(guard);
        assert_eq!(t.states[1].load(Relaxed), DONE);
    }

    #[test]
    fn sizes_and_times_read_naturally() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(duration(27.0), "27s");
        assert_eq!(duration(65.0), "1m 05s");
    }
}
