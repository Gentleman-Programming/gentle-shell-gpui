//! Own the helper and its worker before any command can reach stdin.
use anyhow::Result;
use std::process::{Child, Command, ExitStatus};

pub(super) struct Tree {
    pub child: Child,
    reaped: bool,
    #[cfg(windows)]
    job: Option<windows::Job>,
}

impl Tree {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            let _ = command;
            anyhow::bail!("Voice process containment is unavailable");
        }
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt as _;
                command.process_group(0);
            }
            #[cfg(windows)]
            let job = windows::Job::new()?;
            let mut tree = Self {
                child: command.spawn()?,
                reaped: false,
                #[cfg(windows)]
                job: None,
            };
            #[cfg(windows)]
            {
                job.assign(&tree.child)?;
                tree.job = Some(job);
            }
            #[cfg(unix)]
            let _ = &mut tree;
            Ok(tree)
        }
    }

    pub fn exited(&mut self) -> Result<Option<ExitStatus>> {
        #[cfg(windows)]
        if let Some(status) = self.child.try_wait()? {
            self.job.take();
            self.reaped = true;
            return Ok(Some(status));
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // Observe exit without releasing the PID/PGID. Signal the group
            // while the direct child still reserves the identifier.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: the output points to writable siginfo_t storage and P_PID
            // observes only our child. WNOWAIT leaves the child unreaped.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == -1 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    // Loss of ownership: do not signal a potentially reused PGID.
                    if error.raw_os_error() == Some(libc::ECHILD) {
                        self.reaped = true;
                    }
                    return Err(error.into());
                }
            } else if info.si_signo != 0 {
                self.terminate_tree();
                let status = self.child.wait()?;
                self.reaped = true;
                return Ok(Some(status));
            }
        }
        Ok(None)
    }

    fn terminate_tree(&mut self) {
        #[cfg(windows)]
        drop(self.job.take());
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        // SAFETY: process_group(0) created this group, and we have not reaped
        // the leader. No external waiter is used for this private Child.
        unsafe {
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        if !self.reaped {
            self.terminate_tree();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::io::Result;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::process::Child;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    pub struct Job(OwnedHandle);

    impl Job {
        pub fn new() -> Result<Self> {
            // SAFETY: null arguments request an unnamed non-inheritable job.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: the new job handle is exclusively owned here.
            let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: the structure and size match the information class.
            if unsafe {
                SetInformationJobObject(
                    handle.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self(handle))
        }

        pub fn assign(&self, child: &Child) -> Result<()> {
            // SAFETY: both owned handles remain live throughout the call.
            if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), child.as_raw_handle()) }
                == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
    }
}
