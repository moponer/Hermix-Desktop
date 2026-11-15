//! Orphan-proof containment for the browser process.
//!
//! Pitfall P1 — **job nesting**. Anything hermix spawns is normally already
//! inside hermix' own Job Object (the desktop launcher sets one up). A nested
//! job inherits the outer job's limits, and when the outer job leaves
//! `JOB_OBJECT_LIMIT_BREAKAWAY_OK` unset, a child created with
//! `CREATE_BREAKAWAY_FROM_JOB` fails with `ERROR_ACCESS_DENIED` and Edge never
//! starts at all. So [`launch`] tries breakaway first — that also keeps Edge
//! out of hermix' memory/CPU limits and keeps a stray Edge from taking hermix
//! down when our handle closes — and falls back to a plain spawn on denial.
//! Either way the child is assigned to **our** job, and that assignment is
//! what actually guarantees the no-orphan property.

use std::os::windows::io::AsRawHandle;

use windows::core::{Error, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// A Job Object armed with `KILL_ON_JOB_CLOSE`: **closing the handle kills
/// every process assigned to it**, recursively. That is the whole point — the
/// browser dies with the session whether hermix exits cleanly or is killed.
pub struct Job {
    handle: HANDLE,
}

// `HANDLE` is a bare `*mut c_void`, so it is neither `Send` nor `Sync` by
// derivation. Job objects are kernel-side objects and every call below takes
// `&self`/`&mut self` and is thread-safe; the handle never escapes the type.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// Create an unnamed job. Unnamed on purpose: `CreateJobObjectW` with a
    /// name that already exists returns *that* job rather than failing, which
    /// would silently merge two hermix instances' browsers into one container.
    pub fn new() -> Result<Self, Error> {
        // SAFETY: no name, no security attributes; the returned handle is
        // checked non-null by the binding before it is handed back.
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }?;

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` outlives the call and its size is passed alongside it,
        // so the kernel never reads past the struct.
        unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
        }

        Ok(Self { handle })
    }

    /// Put an already-spawned child into this job.
    ///
    /// There is a race here — the child runs a few instructions before it is
    /// assigned and could spawn its own children first — which is exactly why
    /// the spawn side asks for `CREATE_BREAKAWAY_FROM_JOB`: a grandchild that
    /// escapes before the assignment would outlive us.
    pub fn assign_child(&self, child: &std::process::Child) -> Result<(), Error> {
        // SAFETY: the handle is ours and stays open until `Drop`; the process
        // handle is owned by `child`, which the caller keeps alive.
        unsafe { AssignProcessToJobObject(self.handle, HANDLE(child.as_raw_handle())) }
    }

    /// Kill the whole tree immediately.
    ///
    /// # Safety
    /// Terminating a job is irreversible for every process in it, including
    /// any page the operator was mid-way through filling in. `Drop` already
    /// does this implicitly; this exists for the explicit teardown path.
    pub unsafe fn kill_now(&self) -> Result<(), Error> {
        TerminateJobObject(self.handle, 1)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `handle` is closed exactly once, here, and never used after.
        // The `KILL_ON_JOB_CLOSE` limit set in `new` is what reaps the tree.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// Did this spawn fail because the outer job forbids `CREATE_BREAKAWAY_FROM_JOB`?
///
/// `std::process::Command` reports the raw Win32 error, not an `HRESULT`, so
/// the HRESULT constants in `windows::core` cannot be used to recognise it —
/// this comparison against the raw code is what separates "breakaway denied,
/// retry without the flag" from "that executable does not exist".
pub fn spawn_is_access_denied(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_denied_is_recognised_by_raw_code() {
        assert!(spawn_is_access_denied(&std::io::Error::from_raw_os_error(5)));
        assert!(!spawn_is_access_denied(&std::io::Error::from_raw_os_error(2)));
        // A non-OS error (no raw code at all) is never access-denied.
        assert!(!spawn_is_access_denied(&std::io::Error::other("boom")));
    }

    #[test]
    fn job_is_send_and_sync_so_it_can_live_in_a_static() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Job>();
    }
}
