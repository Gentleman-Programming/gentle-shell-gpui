//! Children that end with the app.
//!
//! On Unix a backend gets its own process group and [`crate::kill_tree`]
//! ends it. On Windows that kill only runs while the app is alive, so a
//! crash or a hard exit left backends, their tools (`engram serve`, started
//! by an extension) and hung probes running. [`contain`] puts such children
//! in one Job Object this process holds and never closes: when the app ends,
//! however it ends, Windows closes the handle and kills every process in
//! the job (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`).
//!
//! Only children handed to [`contain`] join. The app itself stays out, so
//! what it opens for the user (pages and files through the shell, editors,
//! the update relauncher) is never in the job and outlives it.

use std::process::Child;

/// Make `child`, and every process it starts from now on, end when this app
/// ends. Call it right after spawning. Nothing on macOS and Linux, where
/// process groups already cover this. A child that can't join (it already
/// exited, or Windows refused) keeps running as before.
pub fn contain(child: &Child) {
    #[cfg(windows)]
    windows_job::assign(child);
    #[cfg(not(windows))]
    let _ = child;
}

#[cfg(windows)]
mod windows_job {
    use std::os::windows::io::AsRawHandle as _;
    use std::process::Child;
    use std::sync::OnceLock;

    use windows::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows::Win32::System::Threading::GetExitCodeProcess;
    use windows::core::PCWSTR;

    pub(super) fn assign(child: &Child) {
        let Some(job) = job() else { return };
        let process = HANDLE(child.as_raw_handle());
        // SAFETY: both handles are open: the job's for the life of the
        // process, the child's while `child` is borrowed.
        if let Err(error) = unsafe { AssignProcessToJobObject(job, process) }
            && running(process)
        {
            eprintln!(
                "Couldn't tie process {} to the app's lifetime: {error}",
                child.id()
            );
        }
    }

    fn running(process: HANDLE) -> bool {
        let mut code = 0;
        // SAFETY: `process` is an open process handle.
        unsafe { GetExitCodeProcess(process, &mut code) }.is_ok() && code == STILL_ACTIVE.0 as u32
    }

    /// The process-wide job, created on first use. Its handle is never
    /// closed: the process ending closes it, which is what kills the job.
    /// It isn't inheritable, so no child keeps it (and the job) alive.
    fn job() -> Option<HANDLE> {
        // The raw handle value: `HANDLE` itself is not `Sync`.
        static JOB: OnceLock<Option<usize>> = OnceLock::new();
        let job = JOB.get_or_init(|| match create() {
            Ok(job) => Some(job.0 as usize),
            Err(error) => {
                eprintln!("Couldn't create a job to end child processes with the app: {error}");
                None
            }
        });
        job.map(|job| HANDLE(job as *mut _))
    }

    fn create() -> windows::core::Result<HANDLE> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // Breakaway stays allowed: a child may still start something meant to
        // outlive the app with `CREATE_BREAKAWAY_FROM_JOB`.
        info.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        // SAFETY: `info` outlives the call and its size is passed with it; the
        // handle is closed if it can't be configured.
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null())?;
            if let Err(error) = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) {
                let _ = CloseHandle(job);
                return Err(error);
            }
            Ok(job)
        }
    }
}
