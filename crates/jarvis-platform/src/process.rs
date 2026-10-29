//! Process inspection and termination helpers built on `sysinfo` (no unsafe
//! code, no platform `cfg` in callers).

use std::path::Path;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

/// Starts a background process owned by JARVIS, including its descendants.
/// Windows assigns a suspended process to a kill-on-close job before resuming;
/// Unix uses a fresh process group. This is lifecycle containment, not a sandbox.
pub fn spawn_owned(command: std::process::Command) -> std::io::Result<OwnedProcess> {
    use process_wrap::tokio::{CommandWrap, KillOnDrop};
    let mut wrapped = CommandWrap::from(tokio::process::Command::from(command));
    wrapped.wrap(KillOnDrop);
    #[cfg(windows)]
    wrapped
        .wrap(process_wrap::tokio::CreationFlags(
            windows::Win32::System::Threading::CREATE_NO_WINDOW,
        ))
        .wrap(process_wrap::tokio::JobObject);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
    wrapped.spawn().map(|child| OwnedProcess { child })
}

/// Owns the whole job/group even if the root exits before its descendants.
#[derive(Debug)]
pub struct OwnedProcess {
    child: Box<dyn process_wrap::tokio::ChildWrapper>,
}

impl OwnedProcess {
    /// Observe the root's exit without waiting for descendants. Ownership of
    /// the job/group remains here; dropping it then terminates survivors.
    pub async fn wait_root(&mut self) -> std::io::Result<std::process::ExitStatus> {
        // spawn_owned installs exactly one child wrapper (job or group).
        match (self.child.inner_mut() as &mut dyn std::any::Any)
            .downcast_mut::<tokio::process::Child>()
        {
            Some(root) => root.wait().await,
            None => Err(std::io::Error::other("missing native owned process")),
        }
    }
}

impl std::ops::Deref for OwnedProcess {
    type Target = dyn process_wrap::tokio::ChildWrapper;
    fn deref(&self) -> &Self::Target {
        self.child.as_ref()
    }
}

impl std::ops::DerefMut for OwnedProcess {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.child.as_mut()
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn refreshed(pid: u32) -> System {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
        ProcessRefreshKind::nothing()
            .with_memory()
            .with_exe(sysinfo::UpdateKind::Always),
    );
    system
}

/// Resident memory of a process in bytes. Blocking.
pub fn process_memory(pid: u32) -> Option<u64> {
    refreshed(pid)
        .process(Pid::from_u32(pid))
        .map(sysinfo::Process::memory)
}

/// Asks a process to terminate gracefully (SIGTERM). Returns `false` when the
/// platform has no such mechanism (Windows) or the process is gone; callers
/// then fall back to a hard kill.
pub fn request_termination(pid: u32) -> bool {
    refreshed(pid)
        .process(Pid::from_u32(pid))
        .and_then(|process| process.kill_with(Signal::Term))
        .unwrap_or(false)
}

/// Kills `pid` only if it is still running `expected_exe`. Used to clean up a
/// server left behind by a previous session without ever touching an
/// unrelated process that reused the pid. Returns `true` if a process was
/// killed.
pub fn kill_if_executable(pid: u32, expected_exe: &Path) -> bool {
    let system = refreshed(pid);
    let Some(process) = system.process(Pid::from_u32(pid)) else {
        return false;
    };
    let same_exe = match (process.exe(), std::fs::canonicalize(expected_exe)) {
        (Some(exe), Ok(expected)) => {
            std::fs::canonicalize(exe).is_ok_and(|actual| actual == expected)
        }
        _ => false,
    };
    same_exe && process.kill()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_memory_of_the_current_process() {
        assert!(process_memory(std::process::id()).is_some_and(|bytes| bytes > 0));
        assert!(process_memory(u32::MAX - 7).is_none());
    }

    #[test]
    fn never_kills_a_process_with_a_different_executable() {
        // Our own pid with a wrong executable path must be left alone.
        let other = std::env::temp_dir();
        assert!(!kill_if_executable(std::process::id(), &other));
        assert!(!kill_if_executable(u32::MAX - 7, &other));
    }
}
