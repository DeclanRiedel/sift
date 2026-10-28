//! Keep owned migration/server processes within the launcher's OS lifetime.
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::io::{FromRawHandle, IntoRawHandle, OwnedHandle};
use std::ptr::null;

use windows_sys::Win32::System::{
    JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
    Threading::GetCurrentProcess,
};

pub(super) fn own_descendants() -> io::Result<()> {
    // Null security attributes create a non-inheritable handle; null name
    // prevents another process opening this job by name.
    let raw = unsafe { CreateJobObjectW(null(), null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateJobObjectW returned a fresh, uniquely owned valid handle.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: This Win32 information structure contains only numeric fields.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: The pointer and size describe the live, correctly typed structure.
    if unsafe {
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Both handles are valid. No process has yet been spawned by this
    // launcher. Children inherit membership without an assignment race.
    if unsafe { AssignProcessToJobObject(raw, GetCurrentProcess()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // Retain until OS teardown: dropping this handle would kill this launcher
    // too, replacing its intended normal exit status. It is not inherited, so
    // descendants cannot keep the job alive after the launcher exits.
    let _retained_until_process_exit = job.into_raw_handle();
    Ok(())
}
