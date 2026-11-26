//! Moving files between this computer and the tree's server folder: the
//! walks ([`walk_local`], the server's in [`download`]) and the blocking
//! workers ([`upload`], [`download`]) a thread of the transfer's own runs,
//! telling its progress as it goes. Cancelling stops between files (and a
//! download between chunks). The rules are [`gentle_core::transfer`]'s.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gentle_core::transfer::{self, MAX_ENTRIES, Plan, Report, Step, Walked};
use pi_rpc::serve::{EntryKind, ServeClient, ServeError, WRITE_LIMIT};

use crate::features::backend::fs::{self, Fs};

/// Where a transfer is: file `index` of `count`, `done` of `total` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub index: usize,
    pub count: usize,
    pub done: u64,
    pub total: u64,
}

/// What a transfer's thread tells the tree.
pub enum Update {
    Progress(Progress),
    /// It ended: how it went, or why nothing was done.
    Done(Result<Report, String>),
}

type Updates = async_channel::Sender<Update>;

/// The server `path` is on.
pub fn client(path: &Path) -> Result<ServeClient, String> {
    match fs::at(path) {
        Ok(Fs::Server(client)) => Ok(client),
        Ok(Fs::Local) => Err("This folder isn't on a server.".into()),
        Err(error) => Err(error.to_string()),
    }
}

fn too_many() -> String {
    format!("That's more than {MAX_ENTRIES} files and folders: transfer fewer at a time.")
}

fn server_error(error: ServeError) -> String {
    fs::io_error(error).to_string()
}

/// What was walked here, and the folder each dropped name is in.
pub struct Local {
    pub walked: Vec<Walked>,
    pub bases: HashMap<String, PathBuf>,
}

/// Everything under `paths` (files and folders of this computer), each
/// relative to its own folder; links aren't followed. Refused past
/// [`MAX_ENTRIES`]. A second item with a name already dropped is left out.
pub fn walk_local(paths: &[PathBuf]) -> Result<Local, String> {
    fn visit(path: &Path, rel: String, out: &mut Vec<Walked>) -> Result<(), String> {
        if out.len() >= MAX_ENTRIES {
            return Err(too_many());
        }
        let unreadable = |e: std::io::Error| format!("Couldn't read “{}”: {e}", path.display());
        let meta = std::fs::symlink_metadata(path).map_err(unreadable)?;
        if meta.is_dir() {
            out.push(Walked::Dir(rel.clone()));
            let mut names: Vec<_> = std::fs::read_dir(path)
                .map_err(unreadable)?
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect();
            names.sort();
            for name in names {
                let child = format!("{rel}/{}", name.to_string_lossy());
                visit(&path.join(&name), child, out)?;
            }
        } else if meta.is_file() {
            out.push(Walked::File {
                path: rel,
                size: meta.len(),
            });
        } else {
            out.push(Walked::Skipped(rel));
        }
        Ok(())
    }
    let mut local = Local {
        walked: Vec::new(),
        bases: HashMap::new(),
    };
    for path in paths {
        let (Some(name), Some(base)) = (path.file_name(), path.parent()) else {
            continue;
        };
        let name = name.to_string_lossy().into_owned();
        if local.bases.contains_key(&name) {
            continue;
        }
        local.bases.insert(name.clone(), base.to_path_buf());
        visit(path, name, &mut local.walked)?;
    }
    Ok(local)
}

/// An upload ready to go, once its conflicts are settled.
pub struct Upload {
    pub client: ServeClient,
    /// The server folder it goes into.
    pub dest: PathBuf,
    pub bases: HashMap<String, PathBuf>,
    pub plan: Plan,
    /// Dropped names already in `dest`.
    pub conflicts: Vec<String>,
    /// Every name in `dest`.
    pub taken: HashSet<String>,
}

/// Walk what was dropped and look at `dest` (a server folder) for names
/// already there. Blocking.
pub fn prepare_upload(paths: &[PathBuf], dest: &Path) -> Result<Upload, String> {
    let client = client(dest)?;
    let local = walk_local(paths)?;
    let plan = transfer::plan(local.walked, WRITE_LIMIT);
    let listing = client
        .fs_list(&fs::server_path(dest))
        .map_err(server_error)?;
    let mut taken: HashSet<String> = listing.entries.into_iter().map(|e| e.name).collect();
    let tops = plan.top_names();
    // A long listing names only some: ask about the rest one by one.
    if listing.truncated {
        for name in &tops {
            if !taken.contains(name) && client.fs_stat(&fs::server_path(&dest.join(name))).is_ok() {
                taken.insert(name.clone());
            }
        }
    }
    let conflicts = tops.into_iter().filter(|n| taken.contains(n)).collect();
    Ok(Upload {
        client,
        dest: dest.to_path_buf(),
        bases: local.bases,
        plan,
        conflicts,
        taken,
    })
}

/// Carry out `upload`'s plan: folders made, each file read whole right
/// before it is sent. Blocking.
pub fn upload(upload: &Upload, cancel: &AtomicBool, updates: &Updates) -> Report {
    let plan = &upload.plan;
    let mut report = Report {
        count: plan.files(),
        too_large: plan.too_large.clone(),
        skipped: plan.skipped,
        ..Report::default()
    };
    let mut progress = Progress {
        index: 0,
        count: report.count,
        done: 0,
        total: plan.bytes(),
    };
    for step in &plan.steps {
        if cancel.load(Ordering::Acquire) {
            report.cancelled = true;
            break;
        }
        let sent = match step {
            Step::Mkdir(path) => {
                let dir = fs::server_path(&upload.dest.join(path));
                if let Err(error) = upload.client.fs_mkdir(&dir) {
                    report.first_error.get_or_insert(server_error(error));
                }
                continue;
            }
            Step::Put {
                from,
                to,
                size,
                replace,
            } => {
                progress.index += 1;
                let _ = updates.send_blocking(Update::Progress(progress));
                let top = from.split('/').next().unwrap_or(from);
                let local = upload.bases.get(top).map(|base| base.join(from));
                let sent = match local.map(std::fs::read) {
                    None => Err(format!("“{from}” is gone")),
                    Some(Err(error)) => Err(format!("“{from}”: {error}")),
                    Some(Ok(bytes)) if bytes.len() as u64 > WRITE_LIMIT => {
                        report.too_large.push(from.clone());
                        Ok(false)
                    }
                    Some(Ok(bytes)) => upload
                        .client
                        .fs_write(
                            &fs::server_path(&upload.dest.join(to)),
                            &bytes,
                            None,
                            !replace,
                        )
                        .map(|_| true)
                        .map_err(|error| format!("“{to}”: {}", server_error(error))),
                };
                progress.done += size;
                sent
            }
        };
        match sent {
            Ok(true) => report.sent += 1,
            Ok(false) => {}
            Err(error) => {
                report.failed += 1;
                report.first_error.get_or_insert(error);
            }
        }
        let _ = updates.send_blocking(Update::Progress(progress));
    }
    report
}

/// Copy the server's `from` (a folder when `is_dir`) to `to` here. A file
/// replaces what's at `to` (the save dialog asked); a folder becomes a new
/// one there, refused when something is at `to` already. Blocking.
pub fn download(
    client: &ServeClient,
    from: &Path,
    is_dir: bool,
    to: &Path,
    cancel: &AtomicBool,
    updates: &Updates,
) -> Result<Report, String> {
    let server_from = fs::server_path(from);
    if !is_dir {
        let mut report = Report {
            count: 1,
            ..Report::default()
        };
        let finished = fetch(client, &server_from, to, cancel, |done, total| {
            let _ = updates.send_blocking(Update::Progress(Progress {
                index: 1,
                count: 1,
                done,
                total,
            }));
        })?;
        report.sent = usize::from(finished);
        report.cancelled = !finished;
        return Ok(report);
    }
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(format!(
            "“{}” already exists there: choose another name to download the folder to.",
            to.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    let (walked, truncated) = walk_server(client, from)?;
    let plan = transfer::plan(walked, u64::MAX);
    std::fs::create_dir(to).map_err(|e| format!("Couldn't make “{}”: {e}", to.display()))?;
    let mut report = Report {
        count: plan.files(),
        skipped: plan.skipped,
        truncated,
        ..Report::default()
    };
    let mut progress = Progress {
        index: 0,
        count: report.count,
        done: 0,
        total: plan.bytes(),
    };
    for step in &plan.steps {
        if cancel.load(Ordering::Acquire) {
            report.cancelled = true;
            break;
        }
        let fetched = match step {
            Step::Mkdir(path) => {
                let made = transfer::local_path(to, path)
                    .ok_or_else(|| format!("“{path}” can't be a name here"))
                    .and_then(|dir| std::fs::create_dir_all(dir).map_err(|e| e.to_string()));
                if let Err(error) = made {
                    report.first_error.get_or_insert(error);
                }
                continue;
            }
            Step::Put {
                from: path, size, ..
            } => {
                progress.index += 1;
                let before = progress.done;
                let fetched = match transfer::local_path(to, path) {
                    None => Err(format!("“{path}” can't be a name here")),
                    Some(local) => fetch(
                        client,
                        &fs::server_path(&from.join(path)),
                        &local,
                        cancel,
                        |done, _| {
                            progress.done = before + done;
                            let _ = updates.send_blocking(Update::Progress(progress));
                        },
                    ),
                };
                progress.done = before + size;
                fetched
            }
        };
        match fetched {
            Ok(true) => report.sent += 1,
            Ok(false) => report.cancelled = true,
            Err(error) => {
                report.failed += 1;
                report.first_error.get_or_insert(error);
            }
        }
        let _ = updates.send_blocking(Update::Progress(progress));
    }
    Ok(report)
}

/// Everything under the server folder `root`, relative to it: links to
/// files count as files; links to folders, dangling ones and special files
/// are skipped. Also whether some folder listed only part of itself.
fn walk_server(client: &ServeClient, root: &Path) -> Result<(Vec<Walked>, bool), String> {
    let mut out = Vec::new();
    let mut truncated = false;
    let mut pending = vec![String::new()];
    while let Some(rel) = pending.pop() {
        let dir = if rel.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&rel)
        };
        let listing = client
            .fs_list(&fs::server_path(&dir))
            .map_err(server_error)?;
        truncated |= listing.truncated;
        for entry in listing.entries {
            if out.len() >= MAX_ENTRIES {
                return Err(too_many());
            }
            let path = if rel.is_empty() {
                entry.name
            } else {
                format!("{rel}/{}", entry.name)
            };
            match (entry.kind, entry.link_kind) {
                (EntryKind::Dir, _) => {
                    out.push(Walked::Dir(path.clone()));
                    pending.push(path);
                }
                (EntryKind::File, _) | (EntryKind::Symlink, Some(EntryKind::File)) => {
                    out.push(Walked::File {
                        path,
                        size: entry.size,
                    })
                }
                _ => out.push(Walked::Skipped(path)),
            }
        }
    }
    Ok((out, truncated))
}

/// The server's file `from` streamed chunk by chunk into a temporary file
/// beside `to`, then moved over it: `Ok(false)` when cancelled (the
/// temporary file is removed, as on a failure). `progress` gets the bytes so
/// far and the file's size.
fn fetch(
    client: &ServeClient,
    from: &str,
    to: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<bool, String> {
    let temp = part_path(to);
    let result = stream(client, from, &temp, cancel, &mut progress).and_then(|whole| {
        if whole {
            std::fs::rename(&temp, to).map_err(|e| e.to_string())?;
        }
        Ok(whole)
    });
    if !matches!(result, Ok(true)) {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|error| format!("“{}”: {error}", from.rsplit('/').next().unwrap_or(from)))
}

/// [`fetch`]'s chunks written to `temp`: whether all came (not cancelled).
fn stream(
    client: &ServeClient,
    from: &str,
    temp: &Path,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(u64, u64),
) -> Result<bool, String> {
    let mut file = std::fs::File::create_new(temp).map_err(|e| e.to_string())?;
    let mut offset = 0;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok(false);
        }
        let chunk = client
            .fs_read(from, offset, None)
            .map_err(|error| fs::content_error(error, Path::new(from)).to_string())?;
        file.write_all(&chunk.bytes).map_err(|e| e.to_string())?;
        offset += chunk.bytes.len() as u64;
        progress(offset, chunk.size);
        if !chunk.truncated || chunk.bytes.is_empty() {
            return Ok(true);
        }
    }
}

/// `<to>.part-<pid>-<nanos>`: where a download is written until it's whole.
fn part_path(to: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let mut name = to.as_os_str().to_owned();
    name.push(format!(".part-{}-{nanos}", std::process::id()));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_walks_go_deep_and_skip_links() {
        let root = std::env::temp_dir().join(format!("gs-transfer-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("site/css/vendor")).unwrap();
        std::fs::write(root.join("site/index.html"), "<p>").unwrap();
        std::fs::write(root.join("site/css/vendor/a.css"), "a{}").unwrap();
        std::fs::write(root.join("notes.txt"), "hi").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("notes.txt"), root.join("site/link.txt")).unwrap();

        let local = walk_local(&[root.join("site"), root.join("notes.txt")]).unwrap();
        let _ = std::fs::remove_dir_all(&root);

        let mut expected = vec![
            Walked::Dir("site".into()),
            Walked::Dir("site/css".into()),
            Walked::Dir("site/css/vendor".into()),
            Walked::File {
                path: "site/css/vendor/a.css".into(),
                size: 3,
            },
            Walked::File {
                path: "site/index.html".into(),
                size: 3,
            },
        ];
        #[cfg(unix)]
        expected.push(Walked::Skipped("site/link.txt".into()));
        expected.push(Walked::File {
            path: "notes.txt".into(),
            size: 2,
        });
        assert_eq!(local.walked, expected);
        assert_eq!(local.bases.get("site"), Some(&root));
        assert_eq!(local.bases.get("notes.txt"), Some(&root));
    }
}
