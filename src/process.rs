use std::{io, process::Child};

/// Owns a helper process and every process it starts, ending them all when dropped.
pub struct ProcessGuard {
    pub child: Child,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

impl ProcessGuard {
    pub fn new(child: Child) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let mut child = child;
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
            // This job owns only our newly spawned helper and closes its descendants with it.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if job.is_null()
                    || SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        &info as *const _ as *const _,
                        std::mem::size_of_val(&info) as u32,
                    ) == 0
                    || AssignProcessToJobObject(job, child.as_raw_handle()) == 0
                {
                    let error = io::Error::last_os_error();
                    if !job.is_null() {
                        CloseHandle(job);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
                Ok(Self { child, job })
            }
        }
        #[cfg(not(windows))]
        Ok(Self { child })
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        // The handle is owned by this guard and is closed exactly once.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
        #[cfg(unix)]
        // The child starts in a new process group, so this targets only our helper tree.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
