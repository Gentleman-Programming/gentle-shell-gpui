//! Exact Node executables shipped by the supported installers, not a PATH lookup.
use anyhow::{Result, ensure};
use sha2::{Digest as _, Sha256};
use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::path::Path;

// The 100 MiB installer cap is for compressed archives. For their larger
// bin/node entries use the exact observed sizes (not an archive-sized cap).
const WINDOWS_MAX_BYTES: u64 = 100 * 1024 * 1024;

// POSIX: bin/node from the v24.21.0 archives pinned by Gentle Shell v4.9.0.
// Windows: win-x64/node.exe from the official SHASUMS256.txt for v24.21.0
// (bundled) and v24.18.0 (managed).
fn expected(mode: &str) -> Option<(&'static str, u64)> {
    match (std::env::consts::OS, std::env::consts::ARCH, mode) {
        ("macos", "aarch64", "bundled") => Some((
            "e4b5a3af0e05c75de2eae013904145f40fe7fc2a6e6f17510128bf45cca4e79b",
            122_129_232,
        )),
        ("macos", "x86_64", "bundled") => Some((
            "7abcf39bd37ab251015337ff75304d7555f0d8e88c6e0fbf04bce8ce34636f49",
            125_270_960,
        )),
        ("linux", "aarch64", "bundled") => Some((
            "0f8949d1028f6d61506b2d5bc57e7e6fe893d7b1997509b7847294fc9c616584",
            122_893_672,
        )),
        ("linux", "x86_64", "bundled") => Some((
            "7fde7b8afa198da66257f42ee2001d874c7355631e6d1579a5fb5ef1f246df4c",
            126_595_440,
        )),
        ("windows", "x86_64", "bundled") => Some((
            "ba4e6d110e8c1592a1ecd390f6b05f3da124b13871a5be62b341a07a853c6c32",
            WINDOWS_MAX_BYTES,
        )),
        ("windows", "x86_64", "managed") => Some((
            "9a4eb5f1c29c6a2e93852ead46b999e284a6a5ca8bab4d4e241d587d025a52de",
            WINDOWS_MAX_BYTES,
        )),
        _ => None,
    }
}

/// Keep this handle alive until the helper and its worker are gone. On Windows
/// it denies write/delete sharing; Unix still executes by path, so same-user
/// or privileged concurrent changes to the install are outside this guarantee.
pub(super) fn verify(path: &Path, mode: &str) -> Result<File> {
    let (expected, limit) =
        expected(mode).ok_or_else(|| anyhow::anyhow!("No pinned Node for this install"))?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // FILE_SHARE_READ only: execute/read opens can coexist with this guard.
        options.share_mode(1);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    ensure!(dunce::canonicalize(path)? == path, "Node path changed");
    let file = options.open(path)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= limit,
        "Invalid Node file"
    );
    check_path(path, &before)?;
    let mut hash = Sha256::new();
    let mut input = (&file).take(before.len() + 1);
    let mut buf = [0_u8; 32 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    ensure!(
        input.limit() == 1 && format!("{:x}", hash.finalize()) == expected,
        "Unpinned Node executable"
    );
    check_path(path, &before)?;
    Ok(file)
}

fn check_path(path: &Path, before: &Metadata) -> Result<()> {
    ensure!(dunce::canonicalize(path)? == path, "Node path changed");
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: geteuid takes no arguments and has no memory preconditions.
        let uid = unsafe { libc::geteuid() };
        let trusted_owner = |meta: &Metadata| meta.uid() == 0 || meta.uid() == uid;
        let now = std::fs::symlink_metadata(path)?;
        ensure!(
            now.is_file() && trusted_owner(&now) && now.mode() & 0o022 == 0,
            "Unsafe Node file ownership or permissions"
        );
        ensure!(
            now.dev() == before.dev()
                && now.ino() == before.ino()
                && now.len() == before.len()
                && now.ctime() == before.ctime()
                && now.ctime_nsec() == before.ctime_nsec()
                && now.mtime() == before.mtime()
                && now.mtime_nsec() == before.mtime_nsec(),
            "Node file changed"
        );
        for dir in path.ancestors().skip(1) {
            let meta = std::fs::symlink_metadata(dir)?;
            // Sticky directories such as /tmp cannot have their trusted-owned
            // immediate child renamed/deleted by an unrelated user.
            ensure!(
                meta.is_dir()
                    && trusted_owner(&meta)
                    && (meta.mode() & 0o022 == 0 || meta.mode() & 0o1000 != 0),
                "Unsafe Node parent directory"
            );
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = before;
    Ok(())
}
