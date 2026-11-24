//! Code spans in the agent's answers that name a file or folder: which
//! spans may be paths, where one points ([`resolve`]), and how it splits
//! into its folder and its name for display ([`split`]). Only the app
//! knows whether the place exists; a span that looks like a path but
//! names nothing (`origin/dev`, `api.example.com`) stays a code span.

use std::path::{Path, PathBuf};

/// The longest span taken for a path (bytes).
const MAX_LEN: usize = 1024;

/// Where a code span points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanPath {
    /// Absolute: `~` taken to `home`, a relative path joined to the cwd.
    pub path: PathBuf,
    /// 1-based, from a `path:42` (or `path:42:7`, `path:42-50`) suffix.
    pub line: Option<u32>,
}

/// Where the code span `text` would point, or `None` when it can't be a
/// path. A candidate has no whitespace and no URL scheme (`https://`,
/// `git@`), holds a letter or digit, and has a `/` (or `\`), starts with
/// `~` or `.`, or ends in a file extension (`manifest.json`); a bare word
/// (`cargo`, a hash, a lone `~`) is not one. `~/…` resolves under `home`, an
/// absolute path stays as is, anything else joins `cwd`. Lexical: whether
/// it exists is the caller's to check.
pub fn resolve(text: &str, cwd: &Path, home: Option<&Path>) -> Option<SpanPath> {
    if text.is_empty()
        || text.len() > MAX_LEN
        || text.chars().any(|c| c.is_whitespace() || c.is_control())
        || text.contains("://")
        || text.starts_with("git@")
        || !text.chars().any(char::is_alphanumeric)
    {
        return None;
    }
    let (path, line) = split_line(text);
    let pathlike =
        path.contains(['/', '\\']) || path.starts_with(['~', '.']) || has_extension(path);
    if !pathlike {
        return None;
    }
    let path = if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        home?.join(rest)
    } else if path.starts_with('~') {
        // `~user/…`: someone else's home.
        return None;
    } else if is_absolute(path) {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    Some(SpanPath { path, line })
}

/// `text` as its folder and its name, both as written: `build/ui.js` →
/// (`build/`, `ui.js`), `~/Desktop` → (`~/`, `Desktop`), `src/` → (`""`,
/// `src/`). A `:42` line suffix stays with the name.
pub fn split(text: &str) -> (&str, &str) {
    let body = text.trim_end_matches(['/', '\\']);
    match body.rfind(['/', '\\']) {
        Some(at) => text.split_at(at + 1),
        None => ("", text),
    }
}

/// `path:42`, `path:42:7` or `path:42-50` → (`path`, 42). A suffix that
/// isn't a positive line stays part of the path (a drive letter, an odd name).
fn split_line(text: &str) -> (&str, Option<u32>) {
    let line_of =
        |tail: &str| {
            let line = tail.split_once('-').map_or(tail, |(line, end)| {
                if end.parse::<u32>().is_ok() { line } else { "" }
            });
            line.parse::<u32>().ok().filter(|line| *line > 0)
        };
    let Some((head, tail)) = text.rsplit_once(':') else {
        return (text, None);
    };
    let Some(last) = line_of(tail) else {
        return (text, None);
    };
    // `path:line:column`: the line is the one before.
    if !tail.contains('-')
        && let Some((path, line)) = head.rsplit_once(':')
        && let Some(line) = line_of(line).filter(|_| !line.contains('-'))
        && !path.is_empty()
    {
        return (path, Some(line));
    }
    if head.is_empty() {
        return (text, None);
    }
    (head, Some(last))
}

/// Whether the last segment ends in `.ext` (letters or digits after a dot
/// that isn't the first character).
fn has_extension(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(dot) if dot > 0 => {
            let ext = &name[dot + 1..];
            !ext.is_empty() && ext.chars().all(char::is_alphanumeric)
        }
        _ => false,
    }
}

fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with(['/', '\\'])
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Option<(PathBuf, Option<u32>)> {
        resolve(text, Path::new("/work"), Some(Path::new("/home/me")))
            .map(|found| (found.path, found.line))
    }

    #[test]
    fn spans_with_a_slash_a_home_or_a_dot_are_paths() {
        assert_eq!(at("build/ui.js"), Some(("/work/build/ui.js".into(), None)));
        assert_eq!(at("origin/dev"), Some(("/work/origin/dev".into(), None)));
        assert_eq!(at("~/Desktop"), Some(("/home/me/Desktop".into(), None)));
        assert_eq!(at("/etc/hosts"), Some(("/etc/hosts".into(), None)));
        assert_eq!(at("./run.sh"), Some(("/work/./run.sh".into(), None)));
        assert_eq!(
            at(".env.dev.example"),
            Some(("/work/.env.dev.example".into(), None))
        );
        assert_eq!(at("crates/"), Some(("/work/crates/".into(), None)));
    }

    #[test]
    fn bare_names_need_an_extension() {
        assert_eq!(
            at("manifest.json"),
            Some(("/work/manifest.json".into(), None))
        );
        assert_eq!(at("INSTALL.txt"), Some(("/work/INSTALL.txt".into(), None)));
        // Domains look like names with an extension: only the disk tells.
        assert!(at("api-dev.loopops.ai").is_some());
        for word in ["3749610", "cargo", "Makefile", "v1.", "foo.rs()"] {
            assert_eq!(at(word), None, "{word}");
        }
    }

    #[test]
    fn urls_commands_and_symbols_are_not_paths() {
        for text in [
            "https://example.com/a",
            "~",
            "file:///tmp/a",
            "git@github.com:me/repo.git",
            "cargo run -p app",
            "a/b c",
            "~other/file",
            "..",
            "/",
            "~/",
            "",
        ] {
            assert_eq!(at(text), None, "{text:?}");
        }
        assert_eq!(resolve("~/x", Path::new("/work"), None), None);
    }

    #[test]
    fn a_line_suffix_is_kept_apart() {
        assert_eq!(
            at("src/lib.rs:42"),
            Some(("/work/src/lib.rs".into(), Some(42)))
        );
        assert_eq!(
            at("src/lib.rs:42:7"),
            Some(("/work/src/lib.rs".into(), Some(42)))
        );
        assert_eq!(
            at("src/lib.rs:42-50"),
            Some(("/work/src/lib.rs".into(), Some(42)))
        );
        assert_eq!(
            at("src/lib.rs:0"),
            Some(("/work/src/lib.rs:0".into(), None))
        );
        assert_eq!(at("a/b:x"), Some(("/work/a/b:x".into(), None)));
        assert_eq!(at("C:/x/y.rs"), Some(("C:/x/y.rs".into(), None)));
    }

    #[test]
    fn split_keeps_the_text_as_written() {
        assert_eq!(split("build/ui.js"), ("build/", "ui.js"));
        assert_eq!(split("~/Desktop/a.zip"), ("~/Desktop/", "a.zip"));
        assert_eq!(split("~/Desktop"), ("~/", "Desktop"));
        assert_eq!(split("crates/core/"), ("crates/", "core/"));
        assert_eq!(split("src/"), ("", "src/"));
        assert_eq!(split("manifest.json"), ("", "manifest.json"));
        assert_eq!(split("src/lib.rs:42"), ("src/", "lib.rs:42"));
        assert_eq!(split(r"a\b.txt"), (r"a\", "b.txt"));
    }
}
