//! Windows process-tree termination via Job Objects.

use std::io;
use std::ptr;

use tokio::process::{Child, Command};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};

use crate::error::AppError;

/// Nothing to configure before spawn: the Job Object is attached after the
/// child is spawned (see [`ProcessTree::attach`]).
pub fn configure(_cmd: &mut Command) {}

/// Terminate a spawned child immediately. Creates a Job Object, assigns the
/// child, and terminates the job. This is a best-effort one-shot kill; the
/// runner normally holds a [`ProcessTree`] from spawn so descendants are also
/// inside the job.
pub fn terminate(child: &mut Child) {
    if let Ok(mut tree) = ProcessTree::attach(child) {
        tree.terminate();
    }
}

/// A Job Object configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
///
/// The child is assigned to the job at spawn; its descendants inherit job
/// membership automatically. [`ProcessTree::terminate`] terminates every
/// process in the job, and dropping the handle (e.g. when the runner future is
/// cancelled) also kills the tree thanks to `KILL_ON_JOB_CLOSE`.
#[derive(Debug)]
pub struct ProcessTree {
    job: HANDLE,
}

impl ProcessTree {
    /// Create a Job Object and assign `child` to it.
    pub fn attach(child: &Child) -> Result<ProcessTree, AppError> {
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(os_error("CreateJobObjectW failed"));
        }

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            unsafe { CloseHandle(job) };
            return Err(os_error("SetInformationJobObject failed"));
        }

        let raw = child
            .raw_handle()
            .ok_or_else(|| AppError::CliError("child process handle is unavailable".to_string()))?;
        let assigned = unsafe { AssignProcessToJobObject(job, raw) };
        if assigned == 0 {
            unsafe { CloseHandle(job) };
            return Err(os_error("AssignProcessToJobObject failed"));
        }

        Ok(ProcessTree { job })
    }

    /// Terminate every process in the job.
    pub fn terminate(&mut self) {
        if self.job.is_null() {
            return;
        }
        unsafe {
            TerminateJobObject(self.job, 0);
            CloseHandle(self.job);
        }
        self.job = ptr::null_mut();
    }

    /// Mark the tree as already-exited. The job is empty once the child has
    /// exited, so dropping the handle (and closing it) is a harmless no-op.
    pub(crate) fn disarm(&mut self) {}
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if !self.job.is_null() {
            // KILL_ON_JOB_CLOSE terminates any processes still in the job.
            unsafe { CloseHandle(self.job) };
        }
    }
}

fn os_error(what: &str) -> AppError {
    AppError::CliError(format!("{what}: {}", io::Error::last_os_error()))
}
