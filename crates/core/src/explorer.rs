//! Pure rules for the folder explorer (oil.nvim style): which entries of a
//! directory show, in what order, for a filter, and which one is
//! highlighted.

/// One directory entry as the explorer lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// The entries to show for `filter`: folders first, then files, each by name
/// (case-insensitive) while nothing is typed, or by fuzzy score otherwise.
/// Dotfiles stay hidden unless the filter itself starts with a dot.
pub fn visible(entries: &[Entry], filter: &str) -> Vec<Entry> {
    let query = filter.trim().to_lowercase();
    let hidden = query.starts_with('.');
    let mut shown: Vec<(usize, String, &Entry)> = entries
        .iter()
        .filter(|e| hidden == e.name.starts_with('.'))
        .filter_map(|e| {
            Some((
                crate::fuzzy::fuzzy_score(&query, &e.name)?,
                e.name.to_lowercase(),
                e,
            ))
        })
        .collect();
    shown.sort_by(|a, b| (!a.2.is_dir, a.0, &a.1).cmp(&(!b.2.is_dir, b.0, &b.1)));
    shown.into_iter().map(|(_, _, e)| e.clone()).collect()
}

/// The row highlighted once `shown` (`len` rows) is listed: `select` (the
/// row of the folder just left, going up) when there, else the best match
/// while the filter has text (`filtering`), else none, so opening acts on
/// the folder shown, as a folder picker's Open does.
pub fn highlight(select: Option<usize>, filtering: bool, len: usize) -> Option<usize> {
    select
        .filter(|row| *row < len)
        .or_else(|| (filtering && len > 0).then_some(0))
}

/// The row highlighted after moving `delta` rows from `current` among
/// `len`, wrapping; from none, down starts at the first row and up at the
/// last.
pub fn step(current: Option<usize>, delta: isize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let len = len as isize;
    let next = match current {
        Some(row) => (row as isize + delta).rem_euclid(len),
        None if delta > 0 => 0,
        None => len - 1,
    };
    Some(next as usize)
}

#[cfg(test)]
mod tests {
    use super::{Entry, highlight, step, visible};

    fn entry(name: &str, is_dir: bool) -> Entry {
        Entry {
            name: name.into(),
            is_dir,
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn folders_come_first_and_dotfiles_need_a_dot() {
        let entries = [
            entry("readme.md", false),
            entry("src", true),
            entry(".git", true),
            entry("Cargo.toml", false),
            entry("crates", true),
            entry(".env", false),
        ];
        assert_eq!(
            names(&visible(&entries, "")),
            ["crates", "src", "Cargo.toml", "readme.md"]
        );
        assert_eq!(names(&visible(&entries, ".")), [".git", ".env"]);
    }

    #[test]
    fn typing_filters_by_fuzzy_match_keeping_folders_first() {
        let entries = [
            entry("crates", true),
            entry("Cargo.lock", false),
            entry("docs", true),
            entry("cargo-notes", true),
        ];
        assert_eq!(
            names(&visible(&entries, "CAR")),
            ["cargo-notes", "Cargo.lock"]
        );
        assert!(visible(&entries, "zzz").is_empty());
    }

    #[test]
    fn nothing_is_highlighted_until_picked() {
        assert_eq!(highlight(None, false, 3), None);
        // Typing highlights the best match; going up the folder left.
        assert_eq!(highlight(None, true, 3), Some(0));
        assert_eq!(highlight(None, true, 0), None);
        assert_eq!(highlight(Some(2), false, 3), Some(2));
        assert_eq!(highlight(Some(3), false, 3), None);
    }

    #[test]
    fn arrows_start_at_either_end_and_wrap() {
        assert_eq!(step(None, 1, 3), Some(0));
        assert_eq!(step(None, -1, 3), Some(2));
        assert_eq!(step(Some(2), 1, 3), Some(0));
        assert_eq!(step(Some(0), -1, 3), Some(2));
        assert_eq!(step(None, 1, 0), None);
    }
}
