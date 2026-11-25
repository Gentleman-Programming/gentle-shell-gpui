//! Files where work rooted at a folder runs: this computer's, or a Gentle
//! Shell server's (`/api/fs/*`, every path a path there). The views that
//! read and change a project's files go through here, so a server folder is
//! never looked for on this computer. Blocking: call off the UI thread.

use std::io;
use std::path::{Path, PathBuf};

use pi_rpc::serve::{ServeClient, ServeError};

/// The largest file read whole (an editor buffer, a preview).
pub const READ_LIMIT: u64 = 64 * 1024 * 1024;

/// Where `path`'s files are.
pub enum Fs {
    /// This computer's.
    Local,
    /// A Gentle Shell server's, through its client.
    Server(ServeClient),
}

/// Where `path`'s files are: a server's when work there runs on one (an
/// error when this device isn't paired with it any more).
pub fn at(path: &Path) -> io::Result<Fs> {
    match super::server::at(path) {
        None => Ok(Fs::Local),
        Some(Ok(client)) => Ok(Fs::Server(client)),
        Some(Err(error)) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            error.to_string(),
        )),
    }
}

/// `path` as the server reads it.
pub fn server_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A server's error as an I/O error of the matching kind, in its words.
pub fn io_error(error: ServeError) -> io::Error {
    let kind = match (error.status, error.error.as_deref()) {
        (Some(404), _) => io::ErrorKind::NotFound,
        (Some(409), Some("exists")) => io::ErrorKind::AlreadyExists,
        (Some(409), Some("not_empty")) => io::ErrorKind::DirectoryNotEmpty,
        (Some(409), Some("cross_device")) => io::ErrorKind::CrossesDevices,
        (Some(400), Some("not_a_directory")) => io::ErrorKind::NotADirectory,
        (Some(400), Some("is_a_directory")) => io::ErrorKind::IsADirectory,
        (Some(401 | 403), _) => io::ErrorKind::PermissionDenied,
        (Some(413), _) => io::ErrorKind::FileTooLarge,
        (Some(507), _) => io::ErrorKind::StorageFull,
        (None, _) => io::ErrorKind::ConnectionRefused,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error.message)
}

/// What an entry is, as `stat` (links followed) tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub is_dir: bool,
    pub is_file: bool,
    pub len: u64,
    /// Milliseconds since the epoch; 0 when unknown.
    pub modified_ms: u64,
}

/// `path`'s metadata, links followed.
pub fn metadata(path: &Path) -> io::Result<Meta> {
    match at(path)? {
        Fs::Local => {
            let meta = std::fs::metadata(path)?;
            Ok(Meta {
                is_dir: meta.is_dir(),
                is_file: meta.is_file(),
                len: meta.len(),
                modified_ms: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as u64),
            })
        }
        Fs::Server(client) => {
            let stat = client.fs_stat(&server_path(path)).map_err(io_error)?;
            let is_dir = stat.entry.is_dir();
            let is_file = stat.entry.kind == pi_rpc::serve::EntryKind::File
                || (stat.entry.kind == pi_rpc::serve::EntryKind::Symlink
                    && stat.entry.link_kind == Some(pi_rpc::serve::EntryKind::File));
            Ok(Meta {
                is_dir,
                is_file,
                len: stat.entry.size,
                modified_ms: stat.entry.mtime,
            })
        }
    }
}

/// Whether `path` exists (a dangling link doesn't).
pub fn exists(path: &Path) -> bool {
    metadata(path).is_ok()
}

/// Whether `path` is a folder (or a link to one).
pub fn is_dir(path: &Path) -> bool {
    metadata(path).is_ok_and(|m| m.is_dir)
}

/// Whether `path` is a file (or a link to one).
pub fn is_file(path: &Path) -> bool {
    metadata(path).is_ok_and(|m| m.is_file)
}

/// Whether the entry itself exists, a dangling link included.
pub fn entry_exists(path: &Path) -> bool {
    match at(path) {
        Ok(Fs::Local) => path.symlink_metadata().is_ok(),
        Ok(Fs::Server(client)) => client.fs_stat(&server_path(path)).is_ok(),
        Err(_) => false,
    }
}

/// `path` with every link resolved (as `realpath`).
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    match at(path)? {
        Fs::Local => dunce::canonicalize(path),
        Fs::Server(client) => client
            .fs_stat(&server_path(path))
            .map_err(io_error)?
            .real_path
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "a dangling link")),
    }
}

/// `path`'s bytes, refused past [`READ_LIMIT`].
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    match at(path)? {
        Fs::Local => std::fs::read(path),
        Fs::Server(client) => client
            .fs_read_all(&server_path(path), READ_LIMIT)
            .map(|chunk| chunk.bytes)
            .map_err(io_error),
    }
}

/// One entry of a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    /// A folder, or a link to one.
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// The entries of the folder `dir`, unsorted, hidden ones included.
pub fn read_dir(dir: &Path) -> io::Result<Vec<DirEntry>> {
    match at(dir)? {
        Fs::Local => Ok(std::fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|e| {
                let path = e.path();
                let kind = e.file_type().ok();
                let is_symlink = kind.is_some_and(|t| t.is_symlink());
                DirEntry {
                    name: e.file_name().to_string_lossy().into_owned(),
                    is_dir: kind.is_some_and(|t| t.is_dir()) || (is_symlink && path.is_dir()),
                    is_symlink,
                    path,
                }
            })
            .collect()),
        Fs::Server(client) => Ok(client
            .fs_list(&server_path(dir))
            .map_err(io_error)?
            .entries
            .into_iter()
            .map(|e| DirEntry {
                path: dir.join(&e.name),
                is_dir: e.is_dir(),
                is_symlink: e.kind == pi_rpc::serve::EntryKind::Symlink,
                name: e.name,
            })
            .collect()),
    }
}

/// Make the folder `path` and any missing parent.
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    match at(path)? {
        Fs::Local => std::fs::create_dir_all(path),
        Fs::Server(client) => client
            .fs_mkdir(&server_path(path))
            .map(drop)
            .map_err(io_error),
    }
}

/// Make the folder `path` (its parent must exist on this computer; a server
/// makes missing parents too), refused when something is there.
pub fn create_dir(path: &Path) -> io::Result<()> {
    match at(path)? {
        Fs::Local => std::fs::create_dir(path),
        Fs::Server(client) => match client.fs_mkdir(&server_path(path)) {
            Ok(true) => Ok(()),
            Ok(false) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a folder with that name already exists",
            )),
            Err(error) => Err(io_error(error)),
        },
    }
}

/// A new, empty file at `path` (refused when something is there).
pub fn create_new(path: &Path) -> io::Result<()> {
    match at(path)? {
        Fs::Local => std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(drop),
        Fs::Server(client) => client
            .fs_write(&server_path(path), &[], None, true)
            .map(drop)
            .map_err(io_error),
    }
}

/// Move `from` to `to`, refused when `to` exists. A case-only rename on a
/// case-insensitive disk is allowed: both names are the same entry (a server
/// decides that itself).
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    match at(from)? {
        Fs::Local => {
            if to != from && std::fs::symlink_metadata(to).is_ok() && !same_entry(from, to) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "an entry with that name already exists",
                ));
            }
            std::fs::rename(from, to)
        }
        Fs::Server(client) => client
            .fs_rename(&server_path(from), &server_path(to), false)
            .map_err(io_error),
    }
}

/// Whether `a` and `b` name the same entry on this computer's disk.
#[cfg(unix)]
fn same_entry(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Whether `a` and `b` name the same entry on this computer's disk.
#[cfg(not(unix))]
fn same_entry(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Remove a file or link, or a folder with everything in it.
pub fn remove_all(path: &Path) -> io::Result<()> {
    match at(path)? {
        Fs::Local => {
            if std::fs::symlink_metadata(path)?.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
        }
        Fs::Server(client) => client.fs_remove(&server_path(path), true).map_err(io_error),
    }
}

/// Copy a file, or a folder with everything in it, to `to` (which must not
/// exist). On this computer links are copied as links (on Unix). A server
/// makes no links: a link to a file is copied as the file, a link to a
/// folder (which could lead back into itself) or to nothing is refused, and
/// copied files get the server's default permissions.
pub fn copy_all(from: &Path, to: &Path) -> io::Result<()> {
    match at(from)? {
        Fs::Local => copy_local(from, to),
        Fs::Server(client) => {
            let stat = client.fs_stat(&server_path(from)).map_err(io_error)?;
            copy_on_server(&client, from, &stat.entry, to)
        }
    }
}

/// [`copy_all`] on this computer.
fn copy_local(from: &Path, to: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    #[cfg(unix)]
    if meta.file_type().is_symlink() {
        return std::os::unix::fs::symlink(std::fs::read_link(from)?, to);
    }
    if meta.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_local(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(drop)
    }
}

/// [`copy_all`] on a server: `entry` is what `from` is.
fn copy_on_server(
    client: &ServeClient,
    from: &Path,
    entry: &pi_rpc::serve::Entry,
    to: &Path,
) -> io::Result<()> {
    use pi_rpc::serve::EntryKind;
    match (entry.kind, entry.link_kind) {
        (EntryKind::Dir, _) => {
            if !client.fs_mkdir(&server_path(to)).map_err(io_error)? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} already exists", to.display()),
                ));
            }
            let listing = client.fs_list(&server_path(from)).map_err(io_error)?;
            if listing.truncated {
                return Err(io::Error::other(format!(
                    "{} has too many entries to copy",
                    from.display()
                )));
            }
            listing.entries.iter().try_for_each(|child| {
                copy_on_server(
                    client,
                    &from.join(&child.name),
                    child,
                    &to.join(&child.name),
                )
            })
        }
        (EntryKind::File, _) | (EntryKind::Symlink, Some(EntryKind::File)) => {
            let file = client
                .fs_read_all(&server_path(from), READ_LIMIT)
                .map_err(io_error)?;
            client
                .fs_write(&server_path(to), &file.bytes, None, true)
                .map(drop)
                .map_err(io_error)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is a link to a folder or a special file, which a server can't copy",
                from.display()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_errors_keep_their_kind() {
        let error = |status: u16, code: &str| ServeError {
            status: Some(status),
            error: Some(code.into()),
            message: code.into(),
        };
        assert_eq!(
            io_error(error(404, "not_found")).kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            io_error(error(409, "exists")).kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            io_error(error(403, "permission_denied")).kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(io_error(error(409, "exists")).to_string(), "exists");
    }
}
