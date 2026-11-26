//! One directory level of the project tree, honoring `.gitignore` unless
//! ignored entries are asked for: this computer's folders, or a Gentle Shell
//! server's (through its API).
//! Blocking I/O: call off the UI thread.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::features::backend;

/// The most names one `git check-ignore` is asked about: a server takes at
/// most 256 arguments.
const CHECK_BATCH: usize = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// Dotfiles, drawn dimmed.
    pub hidden: bool,
    /// Matched by an ignore file; only listed when asked for, drawn dimmed.
    pub ignored: bool,
}

/// The direct children of `dir`: folders first, then files, each
/// case-insensitively by name. Entries ignored by `.gitignore` (including
/// ignore files in parent directories), `.git/info/exclude`, the global git
/// excludes and `.ignore` are left out unless `show_ignored`, which lists
/// them marked [`Entry::ignored`]. The `.git` folder never is. Hidden files
/// are listed. On a Gentle Shell server git there decides what is ignored
/// (so outside a repository nothing is).
pub fn list_dir(dir: &Path, show_ignored: bool) -> Vec<Entry> {
    let mut entries = if backend::on_server(dir) {
        list_server_dir(dir, show_ignored)
    } else {
        list_local_dir(dir, show_ignored)
    };
    entries.sort_by(compare);
    entries
}

/// [`list_dir`] on this computer, unsorted.
fn list_local_dir(dir: &Path, show_ignored: bool) -> Vec<Entry> {
    let walker = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .ignore(true)
        // Honor `.gitignore` even before `git init`.
        .require_git(false)
        .follow_links(false)
        .build();
    // Symlinks to folders expand like folders.
    let kept = walker
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(|e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir())
                || (e.path_is_symlink() && e.path().is_dir());
            (e.into_path(), is_dir)
        });
    if show_ignored {
        let kept: HashSet<PathBuf> = kept.map(|(path, _)| path).collect();
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|e| {
                let path = e.path();
                let is_dir = e
                    .file_type()
                    .is_ok_and(|t| t.is_dir() || (t.is_symlink() && path.is_dir()));
                let ignored = !kept.contains(&path);
                entry(path, is_dir, ignored)
            })
            .collect()
    } else {
        kept.filter_map(|(path, is_dir)| entry(path, is_dir, false))
            .collect()
    }
}

/// [`list_dir`] on a Gentle Shell server, unsorted: its listing, with what
/// `git check-ignore` there ignores.
fn list_server_dir(dir: &Path, show_ignored: bool) -> Vec<Entry> {
    let Ok(listed) = backend::fs::read_dir(dir) else {
        return Vec::new();
    };
    let listed: Vec<backend::fs::DirEntry> =
        listed.into_iter().filter(|e| e.name != ".git").collect();
    let ignored = ignored_names(dir, &listed);
    listed
        .into_iter()
        .filter_map(|e| {
            let ignored = ignored.contains(&e.name);
            if ignored && !show_ignored {
                return None;
            }
            entry(e.path, e.is_dir, ignored)
        })
        .collect()
}

/// The names among `listed` (entries of `dir`) that git ignores, asked in
/// batches of [`CHECK_BATCH`]. Nothing outside a repository, or when git
/// can't be asked.
fn ignored_names(dir: &Path, listed: &[backend::fs::DirEntry]) -> HashSet<String> {
    let names: Vec<&str> = listed.iter().map(|e| e.name.as_str()).collect();
    let mut ignored = HashSet::new();
    for batch in names.chunks(CHECK_BATCH) {
        // Names are paths, never pathspec magic (`:(glob)…`).
        let Ok(out) = backend::git(dir, &[("GIT_LITERAL_PATHSPECS", "1")])
            .args(["check-ignore", "--"])
            .args(batch)
            .output()
        else {
            break;
        };
        // It fails quietly when none of these is ignored, and says why
        // when it can't tell (not a repository): the next batch can't either.
        if !out.success && !out.stderr.is_empty() {
            break;
        }
        ignored.extend(check_ignore_paths(&String::from_utf8_lossy(&out.stdout)));
    }
    ignored
}

/// The paths `git check-ignore` printed, one a line, unquoted where git
/// quoted them (C-style, for unusual characters).
fn check_ignore_paths(printed: &str) -> impl Iterator<Item = String> + '_ {
    printed.lines().filter(|line| !line.is_empty()).map(|line| {
        line.strip_prefix('"')
            .and_then(|quoted| quoted.strip_suffix('"'))
            .map_or_else(|| line.to_string(), unquote)
    })
}

/// A path git quoted C-style (its inside, without the quotes): `\\`, `\"`,
/// the control escapes and `\ooo` octal bytes.
fn unquote(quoted: &str) -> String {
    let mut bytes = Vec::with_capacity(quoted.len());
    let mut rest = quoted.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        let Some((&escaped, tail)) = rest.split_first() else {
            bytes.push(byte);
            break;
        };
        rest = tail;
        bytes.push(match escaped {
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 0x0b,
            b'0'..=b'7' => {
                let used = rest
                    .iter()
                    .take(2)
                    .take_while(|d| (b'0'..=b'7').contains(*d))
                    .count();
                let value = std::iter::once(&escaped)
                    .chain(&rest[..used])
                    .fold(0u32, |value, digit| value * 8 + u32::from(digit - b'0'));
                rest = &rest[used..];
                value as u8
            }
            other => other,
        });
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn entry(path: PathBuf, is_dir: bool, ignored: bool) -> Option<Entry> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    if name == ".git" {
        return None;
    }
    Some(Entry {
        hidden: name.starts_with('.'),
        path,
        name,
        is_dir,
        ignored,
    })
}

fn compare(a: &Entry, b: &Entry) -> Ordering {
    b.is_dir
        .cmp(&a.is_dir)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.name.cmp(&b.name))
}

#[cfg(test)]
mod tests {
    use super::{check_ignore_paths, list_dir};
    use std::fs;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gentle-files-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &std::path::Path) -> Vec<(String, bool, bool)> {
        list_dir(dir, false)
            .into_iter()
            .map(|e| (e.name, e.is_dir, e.hidden))
            .collect()
    }

    #[test]
    fn folders_first_then_case_insensitive_names() {
        let dir = temp_dir("order");
        for f in ["b.rs", "A.md", "c.toml", ".env"] {
            fs::write(dir.join(f), "x").unwrap();
        }
        for d in ["zeta", "Alpha", ".github"] {
            fs::create_dir(dir.join(d)).unwrap();
        }
        assert_eq!(
            names(&dir),
            vec![
                (".github".into(), true, true),
                ("Alpha".into(), true, false),
                ("zeta".into(), true, false),
                (".env".into(), false, true),
                ("A.md".into(), false, false),
                ("b.rs".into(), false, false),
                ("c.toml".into(), false, false),
            ]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gitignore_applies_here_and_in_subfolders() {
        let dir = temp_dir("ignore");
        fs::write(dir.join(".gitignore"), "target/\n*.log\n/build\n").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(dir.join("target/debug")).unwrap();
        fs::create_dir_all(dir.join("build")).unwrap();
        fs::create_dir_all(dir.join("src/build")).unwrap();
        fs::write(dir.join("debug.log"), "x").unwrap();
        fs::write(dir.join("main.rs"), "x").unwrap();
        fs::write(dir.join("src/lib.rs"), "x").unwrap();
        fs::write(dir.join("src/trace.log"), "x").unwrap();

        assert_eq!(
            names(&dir),
            vec![
                ("src".into(), true, false),
                (".gitignore".into(), false, true),
                ("main.rs".into(), false, false),
            ]
        );
        // A parent's `.gitignore` still applies when listing a subfolder lazily;
        // the anchored `/build` only matches at the root.
        assert_eq!(
            names(&dir.join("src")),
            vec![
                ("build".into(), true, false),
                ("lib.rs".into(), false, false)
            ]
        );
        // Asked for, ignored entries come back marked; `.git` never does.
        let all: Vec<(String, bool)> = list_dir(&dir, true)
            .into_iter()
            .map(|e| (e.name, e.ignored))
            .collect();
        assert_eq!(
            all,
            vec![
                ("build".into(), true),
                ("src".into(), false),
                ("target".into(), true),
                (".gitignore".into(), false),
                ("debug.log".into(), true),
                ("main.rs".into(), false),
            ]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn check_ignore_output_unquotes_what_git_quoted() {
        let printed = "target\na.log\n\"\\303\\261.log\"\n\"q\\\"x\\\\y\\t.log\"\n\n";
        assert_eq!(
            check_ignore_paths(printed).collect::<Vec<_>>(),
            vec!["target", "a.log", "ñ.log", "q\"x\\y\t.log"]
        );
    }
}
