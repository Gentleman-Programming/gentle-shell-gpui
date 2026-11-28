//! The counters behind the resource diagnostics
//! ([`crate::features::diagnostics`]): frames asked for and poll wakeups by
//! source line, renders by view, and the launch the startup steps count
//! from. Off (the default), every hook is one relaxed load of a flag.

use std::collections::BTreeMap;
use std::panic::Location;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

const ENV: &str = "GENTLE_SHELL_GPUI_DIAGNOSTICS";
/// Records from launch, like the variable does.
pub const FLAG: &str = "--diagnostics";

static ENABLED: AtomicBool = AtomicBool::new(false);

/// When the app launched: `main` reads it first.
static LAUNCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Note the launch; called first thing in `main`.
pub fn mark_launch() {
    LazyLock::force(&LAUNCH);
}

/// Milliseconds from the launch to `at`.
pub fn since_launch(at: Instant) -> u64 {
    at.saturating_duration_since(*LAUNCH).as_millis() as u64
}

/// Whether this launch asked to record from the start (the variable or
/// [`FLAG`]).
pub fn requested_at_launch() -> bool {
    std::env::var_os(ENV).is_some_and(|v| !v.is_empty() && v != "0")
        || std::env::args_os().any(|arg| arg == FLAG)
}

/// Whether a recording is running.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Start or stop counting; a start drops what an earlier recording left.
pub fn set_enabled(on: bool) {
    if on {
        take();
    }
    ENABLED.store(on, Ordering::Relaxed);
}

type Site = (&'static str, u32);

struct Counts {
    frames: BTreeMap<Site, u64>,
    renders: BTreeMap<&'static str, u64>,
    ticks: BTreeMap<Site, u64>,
}

static COUNTS: Mutex<Counts> = Mutex::new(Counts {
    frames: BTreeMap::new(),
    renders: BTreeMap::new(),
    ticks: BTreeMap::new(),
});

fn site(at: &'static Location<'static>) -> Site {
    (at.file(), at.line())
}

/// Count a frame asked for at `at` (`ui::request_animation_frame`,
/// `ui::request_frame_in` and the live indicators call it).
pub fn frame_at(at: &'static Location<'static>) {
    if enabled() {
        *COUNTS.lock().unwrap().frames.entry(site(at)).or_default() += 1;
    }
}

/// Count one render of `view`.
pub fn render(view: &'static str) {
    if enabled() {
        *COUNTS.lock().unwrap().renders.entry(view).or_default() += 1;
    }
}

/// Count one wakeup of the poll or timer loop calling it.
#[track_caller]
pub fn tick() {
    if enabled() {
        let at = site(Location::caller());
        *COUNTS.lock().unwrap().ticks.entry(at).or_default() += 1;
    }
}

/// `crates/app/src/shared/ui.rs:728` → `shared/ui.rs:728`, the same on
/// every OS.
fn label((file, line): Site) -> String {
    let file = file.replace('\\', "/");
    let file = file
        .split_once("app/src/")
        .map_or(file.as_str(), |(_, f)| f);
    format!("{file}:{line}")
}

/// The counts since the last call, as (frames, renders, ticks), and reset.
pub fn take() -> [BTreeMap<String, u64>; 3] {
    let mut counts = COUNTS.lock().unwrap();
    let by_site = |map: BTreeMap<Site, u64>| -> BTreeMap<String, u64> {
        map.into_iter().map(|(at, n)| (label(at), n)).collect()
    };
    let frames = by_site(std::mem::take(&mut counts.frames));
    let ticks = by_site(std::mem::take(&mut counts.ticks));
    let renders = std::mem::take(&mut counts.renders)
        .into_iter()
        .map(|(view, n)| (view.to_string(), n))
        .collect();
    [frames, renders, ticks]
}
