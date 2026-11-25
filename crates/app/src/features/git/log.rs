//! The Git pane's read: a branch's commits (or every ref's), how many there
//! are in all, the local branches to pick from, and the repository's state
//! around HEAD: changes, upstream, remotes and a command stopped midway.

use std::path::Path;

use gentle_core::git::avatar::github_repo;
use gentle_core::git::log::{Commit, LOG_ARGS, parse_log};
use gentle_core::git::ops::InProgress;
use gentle_core::git::refs::parse_left_right;

use super::git;
use crate::features::backend;

/// Which commits the pane lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scope {
    /// HEAD's history.
    #[default]
    Head,
    /// One local branch's history.
    Branch(String),
    /// Every branch, remote branch and tag.
    All,
}

/// One read of the Git pane.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    /// Newest first, at most the limit asked for.
    pub commits: Vec<Commit>,
    /// Every commit the scope reaches, loaded or not.
    pub total: usize,
    /// The scope shown: a branch that no longer exists falls back to HEAD.
    pub scope: Scope,
    /// HEAD's branch; `None` on a detached HEAD.
    pub current: Option<String>,
    /// Local branches, most recently committed first.
    pub branches: std::sync::Arc<[String]>,
    /// The work tree has changes not yet committed. Read when HEAD is shown.
    pub dirty: bool,
    /// HEAD's upstream (`origin/main`) and how far HEAD is `(ahead, behind)` it.
    pub upstream: Option<(String, u32, u32)>,
    /// Remote names.
    pub remotes: Vec<String>,
    /// A merge, rebase, cherry-pick or revert waiting on the user.
    pub in_progress: Option<InProgress>,
    /// `(owner, repository)` when the main remote is on GitHub: its API
    /// names the accounts behind commit authors.
    pub github: Option<(String, String)>,
}

impl History {
    /// The branch the list follows, for the header: HEAD's for HEAD.
    pub fn branch(&self) -> Option<&str> {
        match &self.scope {
            Scope::Branch(name) => Some(name),
            Scope::Head => self.current.as_deref(),
            Scope::All => None,
        }
    }
}

/// The newest `limit` commits `scope` reaches. `None` outside a repository;
/// a repository without commits reads as an empty history.
pub fn history(toplevel: &Path, scope: &Scope, limit: usize) -> Option<History> {
    let current = git(toplevel, &["symbolic-ref", "--short", "-q", "HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let branches: Vec<String> = git(
        toplevel,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/heads",
        ],
    )?
    .lines()
    .map(str::to_string)
    .collect();
    // A branch deleted since it was picked falls back to HEAD; HEAD's own
    // branch is HEAD.
    let scope = match scope {
        Scope::Branch(name) if Some(name) == current.as_ref() => Scope::Head,
        Scope::Branch(name) if !branches.contains(name) => Scope::Head,
        other => other.clone(),
    };
    let revs: Vec<&str> = match &scope {
        Scope::Head => vec!["HEAD"],
        Scope::Branch(name) => vec![name.as_str()],
        Scope::All => vec!["--all"],
    };
    let limit = format!("-n{limit}");
    let mut args: Vec<&str> = LOG_ARGS.to_vec();
    args.push(limit.as_str());
    args.extend(&revs);
    args.push("--");
    let commits = git(toplevel, &args)
        .map(|raw| parse_log(&raw))
        .unwrap_or_default();
    let mut count = vec!["rev-list", "--count"];
    count.extend(&revs);
    count.push("--");
    let total = git(toplevel, &count)
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(commits.len());
    let dirty = !matches!(scope, Scope::Branch(_))
        && git(
            toplevel,
            &["status", "--porcelain", "--untracked-files=normal"],
        )
        .is_some_and(|s| !s.trim().is_empty());
    let upstream = git(
        toplevel,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .map(|name| {
        let (ahead, behind) = git(
            toplevel,
            &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        )
        .and_then(|raw| parse_left_right(&raw))
        .unwrap_or((0, 0));
        (name, ahead, behind)
    });
    let remotes: Vec<String> = git(toplevel, &["remote"])
        .map(|raw| raw.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let github = remotes
        .iter()
        .find(|r| *r == "origin")
        .or(remotes.first())
        .and_then(|remote| git(toplevel, &["remote", "get-url", remote]))
        .and_then(|url| github_repo(&url));
    Some(History {
        commits,
        total,
        scope,
        current,
        branches: branches.into(),
        dirty,
        upstream,
        remotes,
        github,
        in_progress: in_progress(toplevel),
    })
}

/// The command git stopped in, from what its directory holds.
pub fn in_progress(toplevel: &Path) -> Option<InProgress> {
    let dir = git(toplevel, &["rev-parse", "--absolute-git-dir"])?;
    let dir = backend::host_path(toplevel, dir.trim());
    InProgress::detect(|name| backend::fs::exists(&dir.join(name)))
}
