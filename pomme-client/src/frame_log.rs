//! Frame times for benchmarks run from a script: the chat lines
//! `#bench start <label>` and `#bench stop` (never sent) bracket a window, and
//! each window's summary is appended to the `POMME_BENCH_LOG` file as a JSON
//! line. Without that variable, chat passes through untouched.

use std::io::Write as _;
use std::sync::Mutex;

struct Window {
    label: String,
    frame_secs: Vec<f32>,
    /// Named per-frame costs in milliseconds, summed over the window.
    phases: std::collections::BTreeMap<&'static str, f64>,
}

static WINDOW: Mutex<Option<Window>> = Mutex::new(None);

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
        *window = Some(Window {
            label: label.trim().to_owned(),
            frame_secs: Vec::new(),
            phases: Default::default(),
        });
    } else if rest.trim() == "stop"
        && let Some(done) = window.take()
    {
        let line = summary(&done);
        tracing::info!("bench {line}");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
    true
}

/// One frame's unclamped interval.
pub fn record(frame_secs: f32) {
    if let Ok(mut window) = WINDOW.try_lock()
        && let Some(window) = window.as_mut()
    {
        window.frame_secs.push(frame_secs);
    }
}

/// Adds `ms` to this window's `phase` total.
pub fn add(phase: &'static str, ms: f32) {
    if let Ok(mut window) = WINDOW.try_lock()
        && let Some(window) = window.as_mut()
    {
        *window.phases.entry(phase).or_default() += f64::from(ms);
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
    let per_frame: serde_json::Map<String, serde_json::Value> = window
        .phases
        .iter()
        .map(|(phase, ms)| ((*phase).to_owned(), (ms / frames.max(1) as f64).into()))
        .collect();
    serde_json::json!({
        "label": window.label,
        "ms_per_frame": per_frame,
        "frames": frames,
        "seconds": total,
        "avg_fps": if total > 0.0 { frames as f32 / total } else { 0.0 },
        "median_fps": fps(at(0.5)),
        "low1_fps": fps(at(0.99)),
        "low01_fps": fps(at(0.999)),
    })
    .to_string()
}
