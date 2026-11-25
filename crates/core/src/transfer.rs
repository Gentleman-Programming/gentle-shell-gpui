//! Moving files between this computer and a Gentle Shell server's folder:
//! what an upload or a download does, in order ([`plan`]), what a name
//! already taken at the destination becomes ([`Conflict`], [`resolve`],
//! [`free_name`]), where a server's entry lands here ([`local_path`]), and
//! what the progress strip and the closing toast say ([`progress_label`],
//! [`Report`]). Paths here are relative and `/`-separated; nothing touches a
//! disk or the network.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// The most entries one transfer walks; a bigger drop or folder is refused.
pub const MAX_ENTRIES: usize = 10_000;

/// One entry a walk found, relative to where the transfer starts (an
/// upload's dropped names included: `photos/a.png`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Walked {
    Dir(String),
    File {
        path: String,
        size: u64,
    },
    /// A link (or a socket, a device…): not transferred.
    Skipped(String),
}

/// What a transfer does, one step at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Make this folder (an existing one is fine).
    Mkdir(String),
    /// Copy the file walked at `from` to `to`; `replace` overwrites a file
    /// already there, else one there is refused.
    Put {
        from: String,
        to: String,
        size: u64,
        replace: bool,
    },
}

/// A transfer's steps: every folder first (parents before children), then
/// the files in the order walked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Files over the size limit, left out (by path).
    pub too_large: Vec<String>,
    /// Links and special files left out.
    pub skipped: usize,
}

impl Plan {
    /// How many files it copies.
    pub fn files(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s, Step::Put { .. }))
            .count()
    }

    /// How many bytes it copies.
    pub fn bytes(&self) -> u64 {
        self.steps
            .iter()
            .map(|s| match s {
                Step::Put { size, .. } => *size,
                Step::Mkdir(_) => 0,
            })
            .sum()
    }

    /// The first component of every step, once each, in order: an upload's
    /// dropped names.
    pub fn top_names(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.steps
            .iter()
            .map(|s| top(step_path(s)))
            .filter(|t| seen.insert(*t))
            .map(str::to_string)
            .collect()
    }

    /// Whether `name` is one of the folders it makes.
    pub fn makes_dir(&self, name: &str) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s, Step::Mkdir(d) if d == name))
    }
}

/// The steps for what a walk found: files over `limit` bytes are left out.
pub fn plan(walked: Vec<Walked>, limit: u64) -> Plan {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut plan = Plan::default();
    for entry in walked {
        match entry {
            Walked::Dir(path) => dirs.push(path),
            Walked::File { path, size } if size > limit => plan.too_large.push(path),
            Walked::File { path, size } => files.push(Step::Put {
                to: path.clone(),
                from: path,
                size,
                replace: false,
            }),
            Walked::Skipped(_) => plan.skipped += 1,
        }
    }
    // A parent is a prefix of its children, so it sorts before them.
    dirs.sort();
    dirs.dedup();
    plan.steps = dirs.into_iter().map(Step::Mkdir).chain(files).collect();
    plan
}

/// What happens to a name already at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// Overwrite it; a folder merges, its other files kept.
    Replace,
    /// Upload under a free name beside it ([`free_name`]).
    KeepBoth,
    /// Leave it, and don't upload this one.
    Skip,
}

/// `plan` with `choice` applied to each of its top-level names in
/// `conflicts`. `taken` is every name at the destination; a kept-both name
/// also avoids the plan's own names and the ones picked before it.
pub fn resolve(
    plan: Plan,
    conflicts: &[String],
    choice: Conflict,
    taken: &HashSet<String>,
) -> Plan {
    let tops: HashSet<String> = plan.top_names().into_iter().collect();
    let mut picked: HashSet<String> = HashSet::new();
    // Old top-level name → new one (`None`: skipped), and whether it replaces.
    let mut renames: Vec<(String, Option<String>)> = Vec::new();
    for name in conflicts.iter().filter(|n| tops.contains(*n)) {
        let new = match choice {
            Conflict::Replace => Some(name.clone()),
            Conflict::KeepBoth => {
                let free = free_name(name, plan.makes_dir(name), |n| {
                    taken.contains(n) || tops.contains(n) || picked.contains(n)
                });
                picked.insert(free.clone());
                Some(free)
            }
            Conflict::Skip => None,
        };
        renames.push((name.clone(), new));
    }
    let steps = plan
        .steps
        .into_iter()
        .filter_map(|step| {
            let Some((old, new)) = renames
                .iter()
                .find(|(old, _)| top(step_path(&step)) == old.as_str())
            else {
                return Some(step);
            };
            let new = new.as_deref()?;
            Some(match step {
                Step::Mkdir(path) => Step::Mkdir(retop(&path, old, new)),
                Step::Put { from, to, size, .. } => Step::Put {
                    to: retop(&to, old, new),
                    from,
                    size,
                    replace: choice == Conflict::Replace,
                },
            })
        })
        .collect();
    Plan { steps, ..plan }
}

/// `name 2.ext`, then `name 3.ext`, …: the first name `taken` says is
/// free. Folders and dotfiles (`.env`) keep their whole name as the stem.
pub fn free_name(name: &str, is_dir: bool, taken: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if !is_dir && dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    (2u32..)
        .map(|n| format!("{stem} {n}{ext}"))
        .find(|candidate| !taken(candidate))
        .expect("a free name")
}

/// Where the server's entry at `path` (relative, `/`-separated) lands
/// under `base`; `None` for an empty path or one that could leave `base`
/// (`..`, a root, a drive, a `\`).
pub fn local_path(base: &Path, path: &str) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let mut out = base.to_path_buf();
    for part in path.split('/') {
        let mut components = Path::new(part).components();
        let single = matches!(
            (components.next(), components.next()),
            (Some(Component::Normal(_)), None)
        );
        if !single || part.contains('\\') || part == "." {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// A path a server's Gentle Shell named or was given (`/export`'s file) as
/// an absolute path there: as is when absolute (`/…`, `C:\…`, `\\…`),
/// under `home` for `~/…` (when known), under `cwd` otherwise, joined with
/// the separator `cwd` uses.
pub fn server_file(path: &str, cwd: &str, home: Option<&str>) -> String {
    let path = path.trim();
    let bytes = path.as_bytes();
    let absolute = path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'));
    if absolute {
        return path.to_string();
    }
    let join = |base: &str, rest: &str| {
        let sep = if base.contains('\\') && !base.contains('/') {
            '\\'
        } else {
            '/'
        };
        let base = base.trim_end_matches(['/', '\\']);
        if rest.is_empty() {
            base.to_string()
        } else {
            format!("{base}{sep}{rest}")
        }
    };
    match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            join(home, rest.trim_start_matches(['/', '\\']))
        }
        _ => join(cwd, path.trim_start_matches("./")),
    }
}

/// `done` of `total` bytes, in whole percent (0 when nothing is known).
pub fn percent(done: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    (u128::from(done.min(total)) * 100 / u128::from(total)) as u8
}

/// The progress strip's words: `Uploading 3 of 12 · 45%`, or for a single
/// file `Downloading “a.zip” · 45%`.
pub fn progress_label(
    verb: &str,
    name: &str,
    index: usize,
    count: usize,
    done: u64,
    total: u64,
) -> String {
    let percent = percent(done, total);
    if count > 1 {
        format!("{verb} {index} of {count} · {percent}%")
    } else {
        format!("{verb} \u{201c}{name}\u{201d} · {percent}%")
    }
}

/// `“a”`, `“a” and “b”`, `“a”, “b”, “c” and 4 more`: at most `shown`
/// names, quoted.
pub fn name_list(names: &[String], shown: usize) -> String {
    let quoted: Vec<String> = names
        .iter()
        .take(shown.max(1))
        .map(|n| format!("\u{201c}{n}\u{201d}"))
        .collect();
    let rest = names.len() - quoted.len();
    match (quoted.split_last(), rest) {
        (None, _) => String::new(),
        (Some((last, [])), 0) => last.clone(),
        (Some((last, init)), 0) => format!("{} and {last}", init.join(", ")),
        (Some(_), rest) => format!("{} and {rest} more", quoted.join(", ")),
    }
}

/// `1 file`, `3 files`.
pub fn files(count: usize) -> String {
    if count == 1 {
        "1 file".into()
    } else {
        format!("{count} files")
    }
}

/// How a transfer went, for its closing toast.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Files copied.
    pub sent: usize,
    /// Files there were to copy.
    pub count: usize,
    /// Files over the size limit, left out.
    pub too_large: Vec<String>,
    /// Links and special files left out.
    pub skipped: usize,
    /// Files (or folders) that failed, and the first failure's words.
    pub failed: usize,
    pub first_error: Option<String>,
    pub cancelled: bool,
    /// A folder listed only some of its entries.
    pub truncated: bool,
}

impl Report {
    /// Everything went: nothing left out, failed or cancelled.
    pub fn clean(&self) -> bool {
        self.too_large.is_empty()
            && self.skipped == 0
            && self.failed == 0
            && self.first_error.is_none()
            && !self.cancelled
            && !self.truncated
    }

    /// What didn't go, sentence by sentence (empty when [`Self::clean`]).
    /// `verb` names the transfer (`upload`); `limit` is the size limit.
    pub fn problems(&self, verb: &str, limit: u64) -> String {
        let mut out = Vec::new();
        if self.cancelled {
            out.push(format!(
                "Cancelled after {} of {}.",
                self.sent,
                files(self.count)
            ));
        }
        if !self.too_large.is_empty() {
            out.push(format!(
                "Skipped {} (over {} MB).",
                name_list(&self.too_large, 3),
                limit / 1024 / 1024
            ));
        }
        match self.skipped {
            0 => {}
            1 => out.push("Skipped 1 link.".into()),
            n => out.push(format!("Skipped {n} links.")),
        }
        if self.truncated {
            out.push("A folder had too many entries to list: some were left out.".into());
        }
        match (self.failed, &self.first_error) {
            (0, Some(error)) => out.push(format!("Couldn't {verb} everything: {error}")),
            (0, None) => {}
            (n, error) => out.push(format!(
                "Couldn't {verb} {}{}",
                files(n),
                error.as_ref().map_or(".".into(), |e| format!(": {e}"))
            )),
        }
        out.join(" ")
    }
}

/// The path a step reads or makes.
fn step_path(step: &Step) -> &str {
    match step {
        Step::Mkdir(path) | Step::Put { from: path, .. } => path,
    }
}

fn top(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

/// `path` with its first component `old` swapped for `new`.
fn retop(path: &str, old: &str, new: &str) -> String {
    match path.strip_prefix(old) {
        Some(rest) => format!("{new}{rest}"),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> Walked {
        Walked::File {
            path: path.into(),
            size,
        }
    }

    fn put(from: &str, to: &str, size: u64, replace: bool) -> Step {
        Step::Put {
            from: from.into(),
            to: to.into(),
            size,
            replace,
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn plans_make_folders_first_and_leave_out_big_files_and_links() {
        let plan = plan(
            vec![
                Walked::Dir("photos".into()),
                file("photos/a.png", 10),
                Walked::Dir("photos/2024".into()),
                file("photos/2024/b.png", 20),
                Walked::Skipped("photos/latest".into()),
                file("movie.mov", 500),
                Walked::Dir("empty".into()),
                file("notes.txt", 5),
            ],
            100,
        );
        assert_eq!(
            plan.steps,
            vec![
                Step::Mkdir("empty".into()),
                Step::Mkdir("photos".into()),
                Step::Mkdir("photos/2024".into()),
                put("photos/a.png", "photos/a.png", 10, false),
                put("photos/2024/b.png", "photos/2024/b.png", 20, false),
                put("notes.txt", "notes.txt", 5, false),
            ]
        );
        assert_eq!(plan.too_large, names(&["movie.mov"]));
        assert_eq!(plan.skipped, 1);
        assert_eq!((plan.files(), plan.bytes()), (3, 35));
        assert_eq!(plan.top_names(), names(&["empty", "photos", "notes.txt"]));
    }

    fn dropped() -> Plan {
        plan(
            vec![
                Walked::Dir("src".into()),
                file("src/main.rs", 1),
                file("a.txt", 2),
                file("b.txt", 3),
            ],
            u64::MAX,
        )
    }

    #[test]
    fn replacing_overwrites_and_merges_folders() {
        let resolved = resolve(
            dropped(),
            &names(&["src", "a.txt"]),
            Conflict::Replace,
            &HashSet::new(),
        );
        assert_eq!(
            resolved.steps,
            vec![
                Step::Mkdir("src".into()),
                put("src/main.rs", "src/main.rs", 1, true),
                put("a.txt", "a.txt", 2, true),
                put("b.txt", "b.txt", 3, false),
            ]
        );
    }

    #[test]
    fn keeping_both_picks_names_free_everywhere() {
        // `a 2.txt` is at the destination, `src 2` is being uploaded.
        let mut plan = dropped();
        plan.steps.push(Step::Mkdir("src 2".into()));
        let taken: HashSet<String> = names(&["src", "a.txt", "a 2.txt"]).into_iter().collect();
        let resolved = resolve(plan, &names(&["src", "a.txt"]), Conflict::KeepBoth, &taken);
        assert_eq!(
            resolved.steps,
            vec![
                Step::Mkdir("src 3".into()),
                put("src/main.rs", "src 3/main.rs", 1, false),
                put("a.txt", "a 3.txt", 2, false),
                put("b.txt", "b.txt", 3, false),
                Step::Mkdir("src 2".into()),
            ]
        );
    }

    #[test]
    fn skipping_leaves_conflicts_out() {
        let resolved = resolve(
            dropped(),
            &names(&["src", "b.txt"]),
            Conflict::Skip,
            &HashSet::new(),
        );
        assert_eq!(resolved.steps, vec![put("a.txt", "a.txt", 2, false)]);
    }

    #[test]
    fn free_names_count_up_from_two() {
        let none = |_: &str| false;
        assert_eq!(free_name("main.rs", false, none), "main 2.rs");
        assert_eq!(free_name("a.tar.gz", false, none), "a.tar 2.gz");
        assert_eq!(free_name(".env", false, none), ".env 2");
        assert_eq!(free_name("v1.2", true, none), "v1.2 2");
        let taken = |n: &str| n == "main 2.rs" || n == "main 3.rs";
        assert_eq!(free_name("main.rs", false, taken), "main 4.rs");
    }

    #[test]
    fn local_paths_stay_under_the_chosen_folder() {
        let base = Path::new("/home/me/Downloads/site");
        assert_eq!(
            local_path(base, "css/main.css"),
            Some(base.join("css").join("main.css"))
        );
        assert_eq!(local_path(base, ".env"), Some(base.join(".env")));
        for bad in ["", "..", "a/../../b", "/etc/passwd", "a//b", "./a", "a\\b"] {
            assert_eq!(local_path(base, bad), None, "{bad}");
        }
    }

    #[test]
    fn progress_reads_as_the_strip_shows_it() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(45, 100), 45);
        assert_eq!(percent(99, 100), 99);
        assert_eq!(percent(150, 100), 100);
        assert_eq!(percent(u64::MAX / 2, u64::MAX), 49);
        assert_eq!(
            progress_label("Uploading", "x", 3, 12, 45, 100),
            "Uploading 3 of 12 · 45%"
        );
        assert_eq!(
            progress_label("Downloading", "a.zip", 1, 1, 1, 4),
            "Downloading \u{201c}a.zip\u{201d} · 25%"
        );
    }

    #[test]
    fn name_lists_shorten_long_ones() {
        assert_eq!(name_list(&names(&["a"]), 3), "“a”");
        assert_eq!(name_list(&names(&["a", "b"]), 3), "“a” and “b”");
        assert_eq!(name_list(&names(&["a", "b", "c"]), 3), "“a”, “b” and “c”");
        assert_eq!(
            name_list(&names(&["a", "b", "c", "d", "e"]), 3),
            "“a”, “b”, “c” and 2 more"
        );
    }

    #[test]
    fn reports_say_what_did_not_go() {
        let clean = Report {
            sent: 2,
            count: 2,
            ..Report::default()
        };
        assert!(clean.clean());
        assert_eq!(clean.problems("upload", 64 << 20), "");
        let rough = Report {
            sent: 3,
            count: 12,
            too_large: names(&["big.iso"]),
            skipped: 2,
            failed: 1,
            first_error: Some("No space left.".into()),
            cancelled: true,
            truncated: false,
        };
        assert!(!rough.clean());
        assert_eq!(
            rough.problems("upload", 64 << 20),
            "Cancelled after 3 of 12 files. Skipped “big.iso” (over 64 MB). \
             Skipped 2 links. Couldn't upload 1 file: No space left."
        );
    }

    #[test]
    fn an_exported_path_is_found_on_the_server() {
        let home = Some("/home/ana");
        // Pi answers its default file relative to the session's folder.
        assert_eq!(
            server_file("gentle-shell-session-1.html", "/home/ana/app", home),
            "/home/ana/app/gentle-shell-session-1.html"
        );
        assert_eq!(
            server_file("./out/a.html", "/home/ana/app/", home),
            "/home/ana/app/out/a.html"
        );
        assert_eq!(server_file("/tmp/a.html", "/x", home), "/tmp/a.html");
        assert_eq!(server_file("~/a.html", "/x", home), "/home/ana/a.html");
        assert_eq!(server_file("~bob/a.html", "/x", home), "/x/~bob/a.html");
        // A Windows server keeps its own separators.
        assert_eq!(
            server_file("a.html", r"C:\Users\ana\app", Some(r"C:\Users\ana")),
            r"C:\Users\ana\app\a.html"
        );
        assert_eq!(
            server_file(r"D:\out\a.html", r"C:\x", None),
            r"D:\out\a.html"
        );
        assert_eq!(
            server_file(r"~\a.html", r"C:\x", Some(r"C:\Users\ana")),
            r"C:\Users\ana\a.html"
        );
    }
}
