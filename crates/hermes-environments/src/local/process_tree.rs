//! A command's whole process tree: a process group on Unix, a job object on
//! Windows. Ending the tree ends every descendant the shell started, which
//! ending the shell alone does not.

use std::io;

use tokio::process::{Child, Command};

#[cfg(unix)]
pub(super) struct ProcessTree {
    pgid: libc::pid_t,
}

#[cfg(windows)]
pub(super) struct ProcessTree {
    job: std::os::windows::io::OwnedHandle,
}

/// Spawns `cmd` as the root of a new process tree.
#[cfg(unix)]
pub(super) fn spawn(cmd: &mut Command) -> io::Result<(Child, ProcessTree)> {
    use std::os::unix::process::CommandExt;

    cmd.as_std_mut().process_group(0);
    let child = cmd.spawn()?;
    let pid = child.id().expect("a child not yet waited on has a pid");
    Ok((
        child,
        ProcessTree {
            pgid: pid as libc::pid_t,
        },
    ))
}

/// Spawns `cmd` as the root of a new process tree. The child starts
/// suspended and runs only once it is in the job, so no descendant can start
/// outside it.
#[cfg(windows)]
pub(super) fn spawn(cmd: &mut Command) -> io::Result<(Child, ProcessTree)> {
    use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

    cmd.creation_flags(hermes_core::subprocess::windows_no_window_creation_flags(
        CREATE_SUSPENDED,
    ));
    let mut child = cmd.spawn()?;
    match ProcessTree::adopt(&child) {
        Ok(tree) => Ok((child, tree)),
        Err(err) => {
            let _ = child.start_kill();
            Err(err)
        }
    }
}

#[cfg(unix)]
impl ProcessTree {
    /// Asks every process in the tree to stop (SIGTERM), waking any that
    /// a terminal read has stopped so they receive it.
    pub(super) fn terminate(&self) -> io::Result<()> {
        self.signal(libc::SIGTERM)?;
        self.signal(libc::SIGCONT)
    }

    /// Stops every process in the tree (SIGKILL).
    pub(super) fn kill(&self) -> io::Result<()> {
        self.signal(libc::SIGKILL)
    }

    fn signal(&self, signal: libc::c_int) -> io::Result<()> {
        // SAFETY: kill(2) with a negative pid signals a process group and
        // touches no memory.
        if unsafe { libc::kill(-self.pgid, signal) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
impl ProcessTree {
    /// Stops every process in the job. Windows has no polite stop for a
    /// console tree, so this is the same as [`ProcessTree::kill`].
    pub(super) fn terminate(&self) -> io::Result<()> {
        self.kill()
    }

    /// Stops every process in the job.
    pub(super) fn kill(&self) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        // SAFETY: the job handle is owned by `self` and open.
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Puts a child spawned with `CREATE_SUSPENDED` into a new job, then
    /// resumes it.
    fn adopt(child: &Child) -> io::Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};

        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("the new process has no handle"))?;
        // SAFETY: null attributes and name ask for an unnamed job with
        // default security.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `job` is a new, open handle that nothing else owns.
        let job = unsafe { OwnedHandle::from_raw_handle(job) };
        // SAFETY: both handles are open for the duration of the call.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("the new process has no id"))?;
        let tree = Self { job };
        if let Err(err) = resume_threads(pid) {
            // A thread resumed before the failure may have started children.
            let _ = tree.kill();
            return Err(err);
        }
        Ok(tree)
    }
}

/// Resumes every thread of process `pid`. Rust's `Command` keeps the handle
/// of the suspended main thread to itself, so the threads are found by
/// snapshot.
#[cfg(windows)]
fn resume_threads(pid: u32) -> io::Result<()> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // SAFETY: a thread snapshot takes no pointers.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `snapshot` is a new, open handle that nothing else owns.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut resumed = false;
    // SAFETY: `entry` is a writable THREADENTRY32 with `dwSize` set.
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: OpenThread takes no pointers.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `thread` is a new, open handle that nothing else owns.
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            // SAFETY: `thread` is open with THREAD_SUSPEND_RESUME access.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            resumed = true;
        }
        // SAFETY: as for Thread32First.
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    if resumed {
        Ok(())
    } else {
        Err(io::Error::other(
            "no thread of the new process was found to resume",
        ))
    }
}
