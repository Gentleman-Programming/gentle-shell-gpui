//! Resource diagnostics: what one sample of the running app records (its
//! own CPU and memory, the frames it asked for, its polls, every tab's
//! backend with the whole process tree under it, and how long each tab's
//! backend took to start), grouping a process table into those trees, and
//! the report that ranks a recording.
//!
//! A sample holds only numbers, source locations and executable names:
//! never prompts, paths, arguments or project names, so a recording can be
//! shared as is.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

/// How many executables each tree lists, by CPU.
pub const TOP: usize = 5;

/// One process as the system reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub parent: Option<u32>,
    /// The executable's name, never its arguments.
    pub name: String,
    /// Percent of one core since the previous sample.
    pub cpu: f32,
    /// Resident memory in bytes.
    pub rss: u64,
}

/// The first line of a recording: the machine and build it came from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub version: String,
    pub os: String,
    pub arch: String,
    pub cores: usize,
    pub memory_mb: f64,
    /// The sampling interval in milliseconds.
    pub interval_ms: u64,
}

/// How [`Header`] is written: `{"header": {…}}`, never mistaken for a sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderLine {
    pub header: Header,
}

/// One line of a recording.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Milliseconds since the Unix epoch.
    pub t: u64,
    pub app: AppSample,
    pub tabs: Vec<TabSample>,
    /// The app's other children (terminals, the device hub, helpers),
    /// each with the processes under it.
    pub others: Vec<Tree>,
}

/// The app process itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSample {
    pub cpu: f32,
    pub rss_mb: f64,
    pub windows: usize,
    /// The active theme and how much its scene moves now (the motion
    /// choice eased by the power state).
    pub theme: String,
    pub motion: String,
    /// Frames asked for per second, by the source line that asked.
    pub frames: BTreeMap<String, f64>,
    /// Renders per second, by view.
    pub renders: BTreeMap<String, f64>,
    /// Poll and timer wakeups per second, by the source line of the loop.
    pub ticks: BTreeMap<String, f64>,
    /// The last login PATH read; absent before the first (and in
    /// recordings older than it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_path: Option<LoginPath>,
}

/// One read of the login PATH (the login shell; the registry on Windows),
/// which a tab's first start waits for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct LoginPath {
    /// When it started, in milliseconds since the app launched.
    pub at_ms: u64,
    pub took_ms: u64,
    /// It gave a PATH; otherwise the inherited one was used.
    pub found: bool,
    /// It ran into its time limit.
    pub timed_out: bool,
}

/// How long a tab's backend took to start. `at_ms` counts from the app's
/// launch; each step's end counts from `at_ms`, absent until it is reached.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Startup {
    pub at_ms: u64,
    /// The launch spec resolved: the login PATH and the launcher's lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ms: Option<u64>,
    /// The backend process spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_ms: Option<u64>,
    /// Its first RPC record arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_record_ms: Option<u64>,
    /// `get_state` answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_ms: Option<u64>,
    /// `get_messages` answered: the backend's conversation shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_ms: Option<u64>,
    /// The resumed conversation showed from its session file, before the
    /// backend's answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_ms: Option<u64>,
}

/// One agent tab: its backend and what it runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TabSample {
    /// Stable for the tab's life: the n-th tab the app opened.
    pub tab: u64,
    pub agents: usize,
    pub agents_running: usize,
    pub jobs: usize,
    pub jobs_running: usize,
    /// Conversation items held in memory.
    pub items: usize,
    /// RPC records and kilobytes received from the backend per second.
    pub rpc_records: f64,
    pub rpc_kb: f64,
    /// The backend and everything under it; `None` while it isn't running.
    pub tree: Option<Tree>,
    /// Its backend's last start; absent before the first (and in
    /// recordings older than it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<Startup>,
}

/// A process and all its descendants.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tree {
    /// The root's executable.
    pub name: String,
    pub procs: usize,
    pub cpu: f32,
    pub rss_mb: f64,
    /// The heaviest executables in it by CPU, each summed over its processes.
    pub top: Vec<Usage>,
}

/// One executable's share of a tree.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub name: String,
    pub procs: usize,
    pub cpu: f32,
    pub rss_mb: f64,
}

/// Bytes to megabytes, rounded to a tenth.
pub fn mb(bytes: u64) -> f64 {
    (bytes as f64 / 1_048_576. * 10.).round() / 10.
}

/// The trees under `app`: every process whose ancestry reaches `app`,
/// grouped by the app's direct child it descends from, keyed by that
/// child's pid. The app itself and unrelated processes are left out.
pub fn trees(processes: &[Process], app: u32) -> BTreeMap<u32, Tree> {
    let by_pid: HashMap<u32, &Process> = processes.iter().map(|p| (p.pid, p)).collect();
    // The direct child of `app` above `pid`, if any. A broken or cyclic
    // parent chain ends the walk.
    let root_of = |pid: u32| {
        let mut current = pid;
        for _ in 0..by_pid.len() {
            let parent = by_pid.get(&current)?.parent?;
            if parent == app {
                return Some(current);
            }
            current = parent;
        }
        None
    };
    let mut members: BTreeMap<u32, Vec<&Process>> = BTreeMap::new();
    for process in processes {
        if process.pid == app {
            continue;
        }
        if let Some(root) = root_of(process.pid) {
            members.entry(root).or_default().push(process);
        }
    }
    members
        .into_iter()
        .map(|(root, procs)| {
            let name = by_pid
                .get(&root)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            (root, tree(name, &procs))
        })
        .collect()
}

fn tree(name: String, procs: &[&Process]) -> Tree {
    let mut by_name: BTreeMap<&str, (usize, f32, u64)> = BTreeMap::new();
    for p in procs {
        let entry = by_name.entry(&p.name).or_default();
        entry.0 += 1;
        entry.1 += p.cpu;
        entry.2 += p.rss;
    }
    let mut top: Vec<Usage> = by_name
        .into_iter()
        .map(|(name, (procs, cpu, rss))| Usage {
            name: name.to_string(),
            procs,
            cpu: round(cpu),
            rss_mb: mb(rss),
        })
        .collect();
    top.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.rss_mb.total_cmp(&a.rss_mb)));
    top.truncate(TOP);
    Tree {
        name,
        procs: procs.len(),
        cpu: round(procs.iter().map(|p| p.cpu).sum()),
        rss_mb: mb(procs.iter().map(|p| p.rss).sum()),
        top,
    }
}

/// Counts over `seconds` as per-second rates, rounded to a tenth.
pub fn rates(counts: &BTreeMap<String, u64>, seconds: f64) -> BTreeMap<String, f64> {
    let seconds = seconds.max(f64::EPSILON);
    counts
        .iter()
        .map(|(key, n)| (key.clone(), ((*n as f64 / seconds) * 10.).round() / 10.))
        .collect()
}

/// A recording's file name for its start time (UTC):
/// `diagnostics-20261118-143005.jsonl`. Names sort by time.
pub fn file_name(unix_ms: u64) -> String {
    let seconds = (unix_ms / 1000) as i64;
    let (year, month, day) =
        crate::usage::time::civil_from_days(seconds.div_euclid(crate::usage::time::DAY));
    let of_day = seconds.rem_euclid(crate::usage::time::DAY);
    format!(
        "diagnostics-{year:04}{month:02}{day:02}-{:02}{:02}{:02}.jsonl",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    )
}

/// The recordings in `names` to delete so `keep` remain, oldest first.
/// Other files in the folder are never listed.
pub fn stale(names: &[String], keep: usize) -> Vec<String> {
    let mut recordings: Vec<&String> = names
        .iter()
        .filter(|n| n.starts_with("diagnostics-") && n.ends_with(".jsonl"))
        .collect();
    recordings.sort();
    let excess = recordings.len().saturating_sub(keep);
    recordings.into_iter().take(excess).cloned().collect()
}

fn round(value: f32) -> f32 {
    (value * 10.).round() / 10.
}

/// Average and peak of one series.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Stat {
    sum: f64,
    peak: f64,
    /// Samples it appeared in; the average is over all samples.
    seen: usize,
}

impl Stat {
    fn add(&mut self, value: f64) {
        self.sum += value;
        self.peak = self.peak.max(value);
        self.seen += 1;
    }

    fn average(&self, samples: usize) -> f64 {
        self.sum / samples.max(1) as f64
    }
}

#[derive(Default)]
struct TabStats {
    cpu: Stat,
    rss: Stat,
    procs: Stat,
    agents: Stat,
    jobs: Stat,
    items: Stat,
    rpc: Stat,
    rpc_kb: Stat,
    heaviest: BTreeMap<String, Stat>,
}

/// A plain-text ranking of a recording (JSONL, one [`Sample`] per line):
/// the app's CPU and memory, which source lines ask for frames and wake
/// polls the most, each tab's process tree, agents and RPC traffic, and how
/// long each tab's backend took to start. Lines that don't parse are
/// counted and skipped.
pub fn report(recording: &str) -> String {
    let mut samples = 0;
    let mut skipped = 0;
    let (mut first, mut last) = (u64::MAX, 0);
    let mut app_cpu = Stat::default();
    let mut app_rss = Stat::default();
    let mut windows = Stat::default();
    let mut frames: BTreeMap<String, Stat> = BTreeMap::new();
    let mut renders: BTreeMap<String, Stat> = BTreeMap::new();
    let mut ticks: BTreeMap<String, Stat> = BTreeMap::new();
    let mut tabs: BTreeMap<u64, TabStats> = BTreeMap::new();
    let mut others: BTreeMap<String, (Stat, Stat)> = BTreeMap::new();
    let mut themes: BTreeMap<String, Stat> = BTreeMap::new();
    let mut header = None;
    // Keyed by start time, the latest sample of each (the most steps).
    let mut login_paths: BTreeMap<u64, LoginPath> = BTreeMap::new();
    let mut startups: BTreeMap<(u64, u64), Startup> = BTreeMap::new();
    for line in recording.lines().filter(|l| !l.trim().is_empty()) {
        if let Ok(line) = serde_json::from_str::<HeaderLine>(line) {
            header = Some(line.header);
            continue;
        }
        let Ok(sample) = serde_json::from_str::<Sample>(line) else {
            skipped += 1;
            continue;
        };
        themes
            .entry(format!("{} ({})", sample.app.theme, sample.app.motion))
            .or_default()
            .add(sample.app.cpu.into());
        samples += 1;
        first = first.min(sample.t);
        last = last.max(sample.t);
        app_cpu.add(sample.app.cpu.into());
        app_rss.add(sample.app.rss_mb);
        windows.add(sample.app.windows as f64);
        if let Some(login) = sample.app.login_path {
            login_paths.insert(login.at_ms, login);
        }
        for (series, values) in [
            (&mut frames, &sample.app.frames),
            (&mut renders, &sample.app.renders),
            (&mut ticks, &sample.app.ticks),
        ] {
            for (key, value) in values {
                series.entry(key.clone()).or_default().add(*value);
            }
        }
        for tab in &sample.tabs {
            if let Some(startup) = tab.startup {
                startups.insert((startup.at_ms, tab.tab), startup);
            }
            let stats = tabs.entry(tab.tab).or_default();
            let tree = tab.tree.clone().unwrap_or_default();
            stats.cpu.add(tree.cpu.into());
            stats.rss.add(tree.rss_mb);
            stats.procs.add(tree.procs as f64);
            stats.agents.add(tab.agents as f64);
            stats.jobs.add(tab.jobs as f64);
            stats.items.add(tab.items as f64);
            stats.rpc.add(tab.rpc_records);
            stats.rpc_kb.add(tab.rpc_kb);
            for usage in &tree.top {
                stats
                    .heaviest
                    .entry(usage.name.clone())
                    .or_default()
                    .add(usage.cpu.into());
            }
        }
        for other in &sample.others {
            let entry = others.entry(other.name.clone()).or_default();
            entry.0.add(other.cpu.into());
            entry.1.add(other.rss_mb);
        }
    }

    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    if let Some(h) = &header {
        line(format!(
            "Gentle Shell GPUI {} on {} {}, {} cores, {:.0} MB",
            h.version, h.os, h.arch, h.cores, h.memory_mb
        ));
    }
    if samples == 0 {
        line(format!("No samples ({skipped} unreadable lines)."));
        return out;
    }
    let seconds = last.saturating_sub(first) / 1000;
    line(format!(
        "{samples} samples over {}m{:02}s{}",
        seconds / 60,
        seconds % 60,
        if skipped > 0 {
            format!(" ({skipped} unreadable lines skipped)")
        } else {
            String::new()
        }
    ));
    line(String::new());
    line("App process".into());
    line(format!(
        "  CPU {:.1}% avg, {:.1}% peak   memory {:.0} MB avg, {:.0} MB peak   windows {:.0} max",
        app_cpu.average(samples),
        app_cpu.peak,
        app_rss.average(samples),
        app_rss.peak,
        windows.peak
    ));
    line(String::new());
    line("App CPU by theme (scene motion)".into());
    let mut ranked: Vec<_> = themes.iter().collect();
    ranked.sort_by(|a, b| b.1.average(b.1.seen).total_cmp(&a.1.average(a.1.seen)));
    for (theme, stat) in ranked {
        line(format!(
            "  {:>6.1}% avg {:>6.1}% peak  {:>3}% of samples  {theme}",
            stat.average(stat.seen),
            stat.peak,
            stat.seen * 100 / samples
        ));
    }
    for (title, series, unit) in [
        ("Frames requested (per second)", &frames, "/s"),
        ("Renders (per second)", &renders, "/s"),
        ("Poll and timer wakeups (per second)", &ticks, "/s"),
    ] {
        line(String::new());
        line(title.into());
        if series.is_empty() {
            line("  none".into());
            continue;
        }
        let mut ranked: Vec<_> = series.iter().collect();
        ranked.sort_by(|a, b| b.1.average(samples).total_cmp(&a.1.average(samples)));
        for (key, stat) in ranked.into_iter().take(10) {
            line(format!(
                "  {:>7.1}{unit} avg {:>7.1}{unit} peak  {:>3}% of samples  {key}",
                stat.average(samples),
                stat.peak,
                stat.seen * 100 / samples
            ));
        }
    }
    line(String::new());
    line("Agent tabs (backend and everything under it)".into());
    if tabs.is_empty() {
        line("  none".into());
    }
    let mut ranked: Vec<_> = tabs.iter().collect();
    ranked.sort_by(|a, b| b.1.cpu.sum.total_cmp(&a.1.cpu.sum));
    for (tab, stats) in ranked {
        let n = stats.cpu.seen;
        line(format!(
            "  tab {tab}: CPU {:.1}% avg, {:.1}% peak   memory {:.0} MB peak   processes {:.0} max",
            stats.cpu.average(n),
            stats.cpu.peak,
            stats.rss.peak,
            stats.procs.peak
        ));
        line(format!(
            "         agents {:.0} max   jobs {:.0} max   items {:.0} max   RPC {:.1} records/s avg ({:.0} peak), {:.1} KB/s avg",
            stats.agents.peak,
            stats.jobs.peak,
            stats.items.peak,
            stats.rpc.average(n),
            stats.rpc.peak,
            stats.rpc_kb.average(n)
        ));
        let mut heaviest: Vec<_> = stats.heaviest.iter().collect();
        heaviest.sort_by(|a, b| b.1.sum.total_cmp(&a.1.sum));
        let names: Vec<String> = heaviest
            .into_iter()
            .take(3)
            .map(|(name, stat)| format!("{name} {:.1}% avg", stat.average(n)))
            .collect();
        if !names.is_empty() {
            line(format!("         heaviest: {}", names.join(", ")));
        }
    }
    line(String::new());
    for text in startup_lines(&login_paths, &startups) {
        line(text);
    }
    line(String::new());
    line("Other child processes".into());
    if others.is_empty() {
        line("  none".into());
    }
    let mut ranked: Vec<_> = others.iter().collect();
    ranked.sort_by(|a, b| b.1.0.sum.total_cmp(&a.1.0.sum));
    for (name, (cpu, rss)) in ranked.into_iter().take(10) {
        line(format!(
            "  {name}: CPU {:.1}% avg, {:.1}% peak   memory {:.0} MB peak",
            cpu.average(samples),
            cpu.peak,
            rss.peak
        ));
    }
    out
}

/// The report's startup section: each login PATH read and each tab start,
/// in the order they began.
fn startup_lines(
    login_paths: &BTreeMap<u64, LoginPath>,
    startups: &BTreeMap<(u64, u64), Startup>,
) -> Vec<String> {
    let mut lines =
        vec!["Backend startup (ms; \"at\" counts from launch, steps from the tab's start)".into()];
    if login_paths.is_empty() && startups.is_empty() {
        lines.push("  not recorded".into());
    }
    for login in login_paths.values() {
        let outcome = if login.timed_out {
            ", timed out: the inherited PATH was used"
        } else if !login.found {
            ", no PATH read: the inherited PATH was used"
        } else {
            ""
        };
        lines.push(format!(
            "  login PATH at +{}: {} ms{outcome}",
            login.at_ms, login.took_ms
        ));
    }
    for ((at, tab), startup) in startups {
        let mut steps: Vec<String> = [
            ("launch spec", startup.resolved_ms),
            ("spawned", startup.spawned_ms),
            ("first record", startup.first_record_ms),
            ("get_state", startup.state_ms),
            ("get_messages", startup.messages_ms),
        ]
        .map(|(step, ms)| match ms {
            Some(ms) => format!("{step} {ms}"),
            None => format!("{step} pending"),
        })
        .into();
        if let Some(ms) = startup.file_ms {
            steps.push(format!("session file {ms}"));
        }
        // Whichever showed the conversation first: its session file, or
        // the backend's answer.
        let shown = match (startup.file_ms, startup.messages_ms) {
            (Some(file), Some(ms)) if ms < file => format!(" (conversation at +{})", at + ms),
            (Some(file), _) => {
                format!(" (conversation at +{}, from its session file)", at + file)
            }
            (None, Some(ms)) => format!(" (conversation at +{})", at + ms),
            (None, None) => String::new(),
        };
        lines.push(format!("  tab {tab} at +{at}: {}{shown}", steps.join(", ")));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, parent: Option<u32>, name: &str, cpu: f32, rss_mb: u64) -> Process {
        Process {
            pid,
            parent,
            name: name.into(),
            cpu,
            rss: rss_mb * 1_048_576,
        }
    }

    #[test]
    fn trees_group_every_descendant_under_the_apps_direct_child() {
        let table = [
            process(1, None, "launchd", 0., 1),
            process(100, Some(1), "gentle-shell-gpui", 30., 500),
            // A tab's backend, its subagents and a build one agent started.
            process(200, Some(100), "gentle-shell", 2., 50),
            process(201, Some(200), "node", 90., 400),
            process(202, Some(201), "node", 40., 300),
            process(203, Some(202), "cargo", 150., 200),
            // A terminal panel's shell.
            process(300, Some(100), "zsh", 1., 5),
            // Not the app's.
            process(400, Some(1), "node", 99., 999),
        ];
        let trees = trees(&table, 100);
        assert_eq!(trees.keys().copied().collect::<Vec<_>>(), [200, 300]);
        let tab = &trees[&200];
        assert_eq!(tab.name, "gentle-shell");
        assert_eq!(tab.procs, 4);
        assert_eq!(tab.cpu, 282.);
        assert_eq!(tab.rss_mb, 950.);
        // Same executables summed, heaviest first.
        let top: Vec<_> = tab
            .top
            .iter()
            .map(|u| (u.name.as_str(), u.procs, u.cpu))
            .collect();
        assert_eq!(
            top,
            [
                ("cargo", 1, 150.),
                ("node", 2, 130.),
                ("gentle-shell", 1, 2.)
            ]
        );
        assert_eq!(trees[&300].procs, 1);
    }

    #[test]
    fn trees_survive_a_cyclic_parent_chain() {
        let table = [
            process(10, Some(11), "a", 1., 1),
            process(11, Some(10), "b", 1., 1),
        ];
        assert!(trees(&table, 100).is_empty());
    }

    #[test]
    fn report_ranks_frame_sources_and_tabs() {
        let sample = |t: u64, backdrop: f64, tab_cpu: f32| Sample {
            t,
            app: AppSample {
                cpu: 40.,
                rss_mb: 600.,
                windows: 1,
                theme: "Aurora".into(),
                motion: "Smooth".into(),
                frames: BTreeMap::from([
                    ("shared/backdrop/mod.rs:382".into(), backdrop),
                    ("shared/ui.rs:728".into(), 2.),
                ]),
                ..Default::default()
            },
            tabs: vec![
                TabSample {
                    tab: 1,
                    agents: 6,
                    tree: Some(Tree {
                        name: "gentle-shell".into(),
                        cpu: tab_cpu,
                        top: vec![Usage {
                            name: "node".into(),
                            procs: 3,
                            cpu: tab_cpu,
                            rss_mb: 100.,
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                TabSample {
                    tab: 2,
                    tree: None,
                    ..Default::default()
                },
            ],
            others: Vec::new(),
        };
        let header = HeaderLine {
            header: Header {
                version: "0.1.23".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                cores: 8,
                memory_mb: 16384.,
                interval_ms: 1000,
            },
        };
        let recording = [
            serde_json::to_string(&header).unwrap(),
            serde_json::to_string(&sample(1_000, 60., 100.)).unwrap(),
            "not json".into(),
            serde_json::to_string(&sample(61_000, 120., 300.)).unwrap(),
        ]
        .join("\n");
        let report = report(&recording);
        assert!(
            report.starts_with(
                "Gentle Shell GPUI 0.1.23 on linux x86_64, 8 cores, 16384 MB\n\
                 2 samples over 1m00s (1 unreadable lines skipped)"
            ),
            "{report}"
        );
        assert!(
            report.contains("40.0% avg   40.0% peak  100% of samples  Aurora (Smooth)"),
            "{report}"
        );
        let backdrop = report.find("shared/backdrop/mod.rs:382").unwrap();
        let live = report.find("shared/ui.rs:728").unwrap();
        assert!(backdrop < live, "busiest frame source first:\n{report}");
        assert!(report.contains("90.0/s avg   120.0/s peak"), "{report}");
        let tab1 = report.find("tab 1: CPU 200.0% avg, 300.0% peak").unwrap();
        assert!(tab1 < report.find("tab 2:").unwrap(), "{report}");
        assert!(report.contains("agents 6 max"), "{report}");
        assert!(report.contains("heaviest: node 200.0% avg"), "{report}");
    }

    #[test]
    fn recordings_are_named_by_utc_time_and_the_oldest_go_first() {
        // 2026-11-18T14:30:05Z
        assert_eq!(
            file_name(1_795_012_205_000),
            "diagnostics-20261118-143005.jsonl"
        );
        let names = [
            "diagnostics-20261118-143005.jsonl",
            "notes.txt",
            "diagnostics-20261101-090000.jsonl",
            "diagnostics-20261117-000000.jsonl",
        ]
        .map(String::from);
        assert_eq!(stale(&names, 2), ["diagnostics-20261101-090000.jsonl"]);
        assert!(stale(&names, 5).is_empty());
    }

    #[test]
    fn rates_divide_by_the_interval() {
        let counts = BTreeMap::from([("a".to_string(), 30), ("b".to_string(), 1)]);
        let rates = rates(&counts, 2.);
        assert_eq!(rates["a"], 15.);
        assert_eq!(rates["b"], 0.5);
    }

    #[test]
    fn report_lists_each_backend_start_step_by_step() {
        let sample = |t: u64, login_path: Option<LoginPath>, tabs: Vec<(u64, Startup)>| Sample {
            t,
            app: AppSample {
                login_path,
                ..Default::default()
            },
            tabs: tabs
                .into_iter()
                .map(|(tab, startup)| TabSample {
                    tab,
                    startup: Some(startup),
                    ..Default::default()
                })
                .collect(),
            others: Vec::new(),
        };
        let login = LoginPath {
            at_ms: 120,
            took_ms: 52,
            found: true,
            timed_out: false,
        };
        let booting = Startup {
            at_ms: 310,
            resolved_ms: Some(60),
            spawned_ms: Some(75),
            ..Default::default()
        };
        let shown = Startup {
            first_record_ms: Some(1320),
            state_ms: Some(1400),
            messages_ms: Some(1410),
            ..booting
        };
        // A restored tab: its session file showed while the backend booted.
        let second_tab = Startup {
            at_ms: 200,
            resolved_ms: Some(5),
            file_ms: Some(40),
            ..Default::default()
        };
        // `/reload` later, after the login PATH was forgotten and read again.
        let restarted = Startup {
            at_ms: 9000,
            resolved_ms: Some(5010),
            spawned_ms: Some(5030),
            ..Default::default()
        };
        let reread = LoginPath {
            at_ms: 9001,
            took_ms: 5003,
            found: false,
            timed_out: true,
        };
        let recording = [
            sample(1_000, Some(login), vec![(1, booting), (2, second_tab)]),
            sample(2_000, Some(login), vec![(1, shown), (2, second_tab)]),
            sample(15_000, Some(reread), vec![(1, restarted)]),
        ]
        .map(|s| serde_json::to_string(&s).unwrap())
        .join("\n");
        let report = report(&recording);
        assert!(
            report.contains(
                "\nBackend startup (ms; \"at\" counts from launch, steps from the tab's start)\n\
                 \x20 login PATH at +120: 52 ms\n\
                 \x20 login PATH at +9001: 5003 ms, timed out: the inherited PATH was used\n\
                 \x20 tab 2 at +200: launch spec 5, spawned pending, first record pending, \
                 get_state pending, get_messages pending, session file 40 \
                 (conversation at +240, from its session file)\n\
                 \x20 tab 1 at +310: launch spec 60, spawned 75, first record 1320, \
                 get_state 1400, get_messages 1410 (conversation at +1720)\n\
                 \x20 tab 1 at +9000: launch spec 5010, spawned 5030, first record pending, \
                 get_state pending, get_messages pending\n\n"
            ),
            "{report}"
        );
    }

    #[test]
    fn a_recording_from_before_startup_steps_still_reports() {
        let recording = r#"{"header":{"version":"0.1.27","os":"macos","arch":"aarch64","cores":10,"memory_mb":16384.0,"interval_ms":1000}}
{"t":1000,"app":{"cpu":10.0,"rss_mb":300.0,"windows":1,"theme":"Aurora","motion":"Smooth","frames":{},"renders":{},"ticks":{}},"tabs":[{"tab":1,"agents":0,"agents_running":0,"jobs":0,"jobs_running":0,"items":4,"rpc_records":2.0,"rpc_kb":1.0,"tree":null}],"others":[]}"#;
        let report = report(recording);
        assert!(report.contains("\n1 samples over 0m00s\n"), "{report}");
        assert!(report.contains("items 4 max"), "{report}");
        assert!(
            report.contains("\nBackend startup (ms; \"at\" counts from launch, steps from the tab's start)\n  not recorded\n"),
            "{report}"
        );
    }
}
