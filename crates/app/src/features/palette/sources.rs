//! What the switcher reads from disk: files under the workspace root (on a
//! Gentle Shell server, through its API) and local git branches; plus
//! `git switch` for the branch rows.

use std::path::Path;

use crate::features::backend;

/// Files under `root`, relative, honoring `.gitignore`; at most `cap`.
/// On a Gentle Shell server, the files git there lists when `root` is in a
/// repository; `None` when it isn't (or git can't be asked): then
/// [`search_files`] finds them by name as the query is typed.
pub(super) fn walk_files(root: &Path, cap: usize) -> Option<Vec<String>> {
    if !backend::on_server(root) {
        return Some(walk_local_files(root, cap));
    }
    let out = git_output(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )
    .ok()
    .filter(|out| out.success)?;
    Some(
        out.stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .take(cap)
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect(),
    )
}

/// [`walk_files`] on this computer.
fn walk_local_files(root: &Path, cap: usize) -> Vec<String> {
    ignore::WalkBuilder::new(root)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .take(cap)
        .collect()
}

/// Files under `root` on its Gentle Shell server whose names match `query`
/// (`GET /api/fs/search`, best first), relative; at most `cap`. Empty when
/// the server can't be asked.
pub(super) fn search_files(root: &Path, query: &str, cap: usize) -> Vec<String> {
    let Some(Ok(client)) = backend::server::at(root) else {
        return Vec::new();
    };
    let Ok(hits) = client.fs_search(&backend::fs::server_path(root), query, cap) else {
        return Vec::new();
    };
    hits.into_iter()
        .filter(|hit| hit.kind == pi_rpc::serve::EntryKind::File)
        .filter_map(|hit| {
            Path::new(&hit.path)
                .strip_prefix(root)
                .ok()
                .map(|rel| rel.to_string_lossy().into_owned())
        })
        .collect()
}

/// `git` in `root` with `args`, its output captured.
fn git_output(root: &Path, args: &[&str]) -> std::io::Result<crate::features::backend::GitOutput> {
    crate::features::backend::git(root, &[]).args(args).output()
}

/// Local branches, `(name, is_current)`; empty outside a repository.
pub(super) fn list_branches(root: &Path) -> Vec<(String, bool)> {
    let Ok(out) = git_output(root, &["branch", "--format=%(HEAD)%(refname:short)"]) else {
        return Vec::new();
    };
    if !out.success {
        return Vec::new();
    }
    parse_branches(&String::from_utf8_lossy(&out.stdout))
}

fn parse_branches(raw: &str) -> Vec<(String, bool)> {
    raw.lines()
        .filter_map(|line| {
            let (head, name) = line.split_at_checked(1)?;
            let name = name.trim();
            (!name.is_empty() && !name.starts_with('(')).then(|| (name.to_string(), head == "*"))
        })
        .collect()
}

/// `git switch <branch>` in `root`; the error is git's own message.
pub fn switch_branch(root: &Path, branch: &str) -> Result<(), String> {
    let out = git_output(root, &["switch", branch]).map_err(|e| e.to_string())?;
    if out.success {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_branches;

    #[test]
    fn branches_parse_with_the_current_one_marked() {
        let raw = "*main\n feature/x\n (HEAD detached at 1234)\n\n";
        assert_eq!(
            parse_branches(raw),
            vec![("main".to_string(), true), ("feature/x".to_string(), false)]
        );
    }
}
