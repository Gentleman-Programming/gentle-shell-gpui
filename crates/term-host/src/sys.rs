//! What differs between Unix and Windows outside the PTY: owner-only folders
//! and files, and the system's random bytes.

use std::fs::File;
use std::path::Path;

/// Fills `bytes` from the system's CSPRNG.
pub fn random_bytes(bytes: &mut [u8]) -> std::io::Result<()> {
    imp::random_bytes(bytes)
}

/// Creates `dir` (and its parents). On Unix it ends up owner-only (0700) and
/// must be ours; on Windows it keeps the ACL it inherits (the user's profile
/// is private already), and what needs protecting is created owner-only.
pub fn private_dir(dir: &Path) -> Result<(), String> {
    imp::private_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))
}

/// Creates the file `path` (which must not exist) readable and writable only
/// by its owner, from the start: 0600 on Unix, a protected DACL that grants
/// only the owner on Windows.
pub fn private_file(path: &Path) -> std::io::Result<File> {
    imp::private_file(path)
}

/// Opens `path` to append, creating it owner-only where the system allows.
pub fn append_file(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Compares secrets in time that doesn't depend on where they differ.
pub fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(unix)]
mod imp {
    use std::fs::{DirBuilder, File, Permissions};
    use std::io::{Error, ErrorKind, Read as _};
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    use std::path::Path;

    pub fn random_bytes(bytes: &mut [u8]) -> std::io::Result<()> {
        File::open("/dev/urandom")?.read_exact(bytes)
    }

    pub fn private_dir(dir: &Path) -> std::io::Result<()> {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let meta = std::fs::metadata(dir)?;
        // SAFETY: getuid(2) can't fail.
        if meta.uid() != unsafe { libc::getuid() } {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "belongs to another user",
            ));
        }
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    pub fn private_file(path: &Path) -> std::io::Result<File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::FromRawHandle as _;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL};

    /// Protected (nothing inherited), full access for the owner only.
    const OWNER_ONLY: &str = "D:P(A;;FA;;;OW)";

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    pub fn random_bytes(bytes: &mut [u8]) -> std::io::Result<()> {
        for chunk in bytes.chunks_mut(u32::MAX as usize) {
            // SAFETY: the buffer and its length are `chunk`'s.
            let status = unsafe {
                BCryptGenRandom(
                    std::ptr::null_mut(),
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                )
            };
            if status < 0 {
                return Err(std::io::Error::other(format!(
                    "BCryptGenRandom failed: {status:#x}"
                )));
            }
        }
        Ok(())
    }

    pub fn private_dir(dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)
    }

    pub fn private_file(path: &Path) -> std::io::Result<File> {
        let sddl = wide(std::ffi::OsStr::new(OWNER_ONLY));
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; `descriptor` is freed below.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let name = wide(path.as_os_str());
        // SAFETY: `name` is NUL-terminated and `attributes` outlives the call.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                0,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        let error = std::io::Error::last_os_error();
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(descriptor) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(error);
        }
        // SAFETY: a file handle CreateFileW just opened, owned from here.
        Ok(unsafe { File::from_raw_handle(handle) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_secret_compares_whole_values() {
        assert!(same_secret(b"abc", b"abc"));
        assert!(!same_secret(b"abc", b"abd"));
        assert!(!same_secret(b"abc", b"ab"));
        assert!(same_secret(b"", b""));
    }

    #[test]
    fn random_bytes_are_random() {
        let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
        random_bytes(&mut a).unwrap();
        random_bytes(&mut b).unwrap();
        assert_ne!(a, b);
    }
}
