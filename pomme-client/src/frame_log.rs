//! Frame times for benchmarks run from a script: the chat lines
//! `#bench start <label>` and `#bench stop` (never sent) bracket a window, and
//! each window's summary is appended to the `POMME_BENCH_LOG` file as a JSON
//! line. Without that variable, chat passes through untouched.
//!
//! `POMME_BENCH_AUTO=<label>,<start>,<seconds>,<count>` also opens `count`
//! windows of `seconds` each, back to back, from `start` seconds after the
//! first frame, for a live client nothing can type into.
//!
//! `POMME_AUTO_SCREENSHOTS=<seconds>,...` takes an F2 screenshot that many
//! seconds into the first world joined, likewise.
//!
//! Each window also keeps the phases of its slow frames (over
//! [`slow_frame_secs`]) apart, to show what a frame-time spike was spent on.

use std::io::Write as _;
use std::sync::Mutex;

struct Window {
    label: String,
    frame_secs: Vec<f32>,
    /// Named per-frame costs in milliseconds, summed over the window.
    phases: std::collections::BTreeMap<&'static str, f64>,
    /// The same, summed over the slow frames only.
    slow_phases: std::collections::BTreeMap<&'static str, f64>,
    slow_frames: usize,
    /// The frame in progress's phases, until it ends.
    frame: Vec<(&'static str, f32)>,
}

impl Window {
    fn new(label: &str) -> Self {
        Self {
            label: label.trim().to_owned(),
            frame_secs: Vec::new(),
            phases: Default::default(),
            slow_phases: Default::default(),
            slow_frames: 0,
            frame: Vec::new(),
        }
    }
}

/// A frame slower than 150 FPS counts as slow, unless `POMME_BENCH_SLOW_FPS`
/// names another rate (to split out the frames behind the 1% lows).
fn slow_frame_secs() -> f32 {
    static SECS: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *SECS.get_or_init(|| {
        let fps = std::env::var("POMME_BENCH_SLOW_FPS")
            .ok()
            .and_then(|fps| fps.parse::<f32>().ok())
            .filter(|fps| *fps > 0.0)
            .unwrap_or(150.0);
        1.0 / fps
    })
}

static WINDOW: Mutex<Option<Window>> = Mutex::new(None);

/// `POMME_BENCH_AUTO`'s schedule, timed from the first frame.
struct Auto {
    label: String,
    start: f32,
    seconds: f32,
    count: u32,
    first_frame: std::time::Instant,
    /// Windows opened so far.
    opened: u32,
}

static AUTO: Mutex<Option<Auto>> = Mutex::new(None);

/// `POMME_AUTO_SCREENSHOTS`'s times still to come, soonest last, and when
/// the first in-game frame asked.
static SHOTS: Mutex<Option<(Vec<f32>, std::time::Instant)>> = Mutex::new(None);
static SHOTS_READ: std::sync::Once = std::sync::Once::new();

/// Whether a scheduled screenshot is due; each is due once.
pub fn screenshot_due() -> bool {
    SHOTS_READ.call_once(|| {
        let times = std::env::var("POMME_AUTO_SCREENSHOTS").ok().map(|spec| {
            let mut times: Vec<f32> = spec
                .split(',')
                .filter_map(|t| t.trim().parse().ok())
                .collect();
            times.sort_by(|a, b| b.total_cmp(a));
            times
        });
        *SHOTS.lock().unwrap_or_else(|e| e.into_inner()) =
            times.map(|times| (times, std::time::Instant::now()));
    });
    let mut shots = SHOTS.lock().unwrap_or_else(|e| e.into_inner());
    let Some((times, start)) = shots.as_mut() else {
        return false;
    };
    let elapsed = start.elapsed().as_secs_f32();
    if times.last().is_some_and(|&t| t <= elapsed) {
        times.pop();
        return true;
    }
    false
}
static AUTO_READ: std::sync::Once = std::sync::Once::new();

fn read_auto() -> Option<Auto> {
    let spec = std::env::var("POMME_BENCH_AUTO").ok()?;
    let mut parts = spec.split(',');
    let label = parts.next()?.trim().to_owned();
    let start = parts.next()?.trim().parse().ok()?;
    let seconds = parts.next()?.trim().parse().ok()?;
    let count = parts.next().map_or(Some(1), |c| c.trim().parse().ok())?;
    Some(Auto {
        label,
        start,
        seconds,
        count,
        first_frame: std::time::Instant::now(),
        opened: 0,
    })
}

/// Opens and closes `POMME_BENCH_AUTO`'s windows on schedule.
fn run_auto() {
    AUTO_READ.call_once(|| *AUTO.lock().unwrap_or_else(|e| e.into_inner()) = read_auto());
    let mut auto = AUTO.lock().unwrap_or_else(|e| e.into_inner());
    let Some(a) = auto.as_mut() else {
        return;
    };
    let elapsed = a.first_frame.elapsed().as_secs_f32() - a.start;
    if elapsed < 0.0 {
        return;
    }
    // Window `n` (from 0) runs from `n * seconds`; one past the last stops.
    let current = (elapsed / a.seconds) as u32;
    if current >= a.count {
        handle_chat("#bench stop");
        *auto = None;
        return;
    }
    if a.opened <= current {
        handle_chat(&format!("#bench start {}#{}", a.label, current + 1));
        a.opened = current + 1;
    }
}

fn log_path() -> Option<std::path::PathBuf> {
    std::env::var_os("POMME_BENCH_LOG").map(Into::into)
}

/// Starts or stops a window for a `#bench` chat line; whether it was one.
pub fn handle_chat(message: &str) -> bool {
    let Some(rest) = message.strip_prefix("#bench ") else {
        return false;
    };
    let Some(path) = log_path() else {
        return false;
    };
    let mut window = WINDOW.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(label) = rest.strip_prefix("start ") {
        if let Some(done) = window.take() {
            finish(&done, &path);
        }
        *window = Some(Window::new(label));
    } else if rest.trim() == "stop"
        && let Some(done) = window.take()
    {
        finish(&done, &path);
    }
    true
}

/// Logs a window's summary and appends it to the bench log.
fn finish(window: &Window, path: &std::path::Path) {
    let line = summary(window);
    tracing::info!("bench {line}");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// One frame's unclamped interval; the phases added since the last call are
/// that frame's.
pub fn record(frame_secs: f32) {
    if log_path().is_some() {
        run_auto();
    }
    if let Ok(mut window) = WINDOW.try_lock()
        && let Some(window) = window.as_mut()
    {
        window.frame_secs.push(frame_secs);
        let slow = frame_secs > slow_frame_secs();
        window.slow_frames += usize::from(slow);
        for (phase, ms) in std::mem::take(&mut window.frame) {
            *window.phases.entry(phase).or_default() += f64::from(ms);
            if slow {
                *window.slow_phases.entry(phase).or_default() += f64::from(ms);
            }
        }
    }
}

thread_local! {
    static MUTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Drops this thread's [`add`]s: its work isn't part of any frame (the light
/// worker's runs).
pub fn mute_this_thread() {
    MUTED.with(|muted| muted.set(true));
}

/// Adds `ms` to this frame's `phase`.
pub fn add(phase: &'static str, ms: f32) {
    if MUTED.with(std::cell::Cell::get) {
        return;
    }
    if let Ok(mut window) = WINDOW.try_lock()
        && let Some(window) = window.as_mut()
    {
        window.frame.push((phase, ms));
    }
}

/// Average and median FPS and the 1% low (the 99th percentile frame time).
fn summary(window: &Window) -> String {
    let mut sorted = window.frame_secs.clone();
    sorted.sort_by(f32::total_cmp);
    let frames = sorted.len();
    let total: f32 = sorted.iter().sum();
    let at = |q: f32| sorted.get(((frames as f32 * q) as usize).min(frames.saturating_sub(1)));
    let fps = |secs: Option<&f32>| secs.map_or(0.0, |s| 1.0 / s.max(1e-6));
    let per = |phases: &std::collections::BTreeMap<&'static str, f64>, n: usize| {
        phases
            .iter()
            .map(|(phase, ms)| ((*phase).to_owned(), (ms / n.max(1) as f64).into()))
            .collect::<serde_json::Map<String, serde_json::Value>>()
    };
    let per_frame = per(&window.phases, frames);
    let per_slow_frame = per(&window.slow_phases, window.slow_frames);
    serde_json::json!({
        "label": window.label,
        "ms_per_frame": per_frame,
        "slow_frames": window.slow_frames,
        "ms_per_slow_frame": per_slow_frame,
        "frames": frames,
        "seconds": total,
        "avg_fps": if total > 0.0 { frames as f32 / total } else { 0.0 },
        "median_fps": fps(at(0.5)),
        "low1_fps": fps(at(0.99)),
        "low01_fps": fps(at(0.999)),
    })
    .to_string()
}
