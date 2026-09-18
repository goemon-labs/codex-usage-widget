use std::{
    io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub struct Instance {
    signal: Arc<os::Signal>,
    stopping: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
    // Release ownership after the listener and its endpoint have been cleaned up.
    _guard: os::Guard,
}

impl Instance {
    pub fn acquire() -> io::Result<Option<Self>> {
        let dirs = directories::ProjectDirs::from("", "", "codex-usage-widget")
            .ok_or_else(|| io::Error::other("アプリの保存先を取得できませんでした"))?;
        Self::acquire_at(dirs.data_local_dir())
    }

    fn acquire_at(directory: &Path) -> io::Result<Option<Self>> {
        // Stable per-user name; only used to name local OS synchronization objects.
        let id = directory
            .to_string_lossy()
            .bytes()
            .fold(0xcbf29ce484222325u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            });
        let (primary, guard, signal) = os::acquire(directory, id)?;
        if !primary {
            os::allow_activation();
            // A simultaneous first launch may still be creating its Unix endpoint.
            for attempt in 0..40 {
                match signal.notify() {
                    Ok(()) => return Ok(None),
                    Err(error)
                        if attempt < 39
                            && matches!(
                                error.kind(),
                                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                            ) =>
                    {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => return Err(error),
                }
            }
            unreachable!();
        }
        Ok(Some(Self {
            signal: Arc::new(signal),
            stopping: Arc::new(AtomicBool::new(false)),
            watcher: None,
            _guard: guard,
        }))
    }

    pub fn listen(&mut self, on_show: impl Fn() + Send + 'static) -> io::Result<()> {
        let signal = self.signal.clone();
        let stopping = self.stopping.clone();
        self.watcher = Some(
            thread::Builder::new()
                .name("widget-activation".into())
                .spawn(move || {
                    // Sleep in the OS until another launch requests activation; no polling loop.
                    while signal.wait().is_ok() {
                        if stopping.load(Ordering::Relaxed) {
                            break;
                        }
                        on_show();
                    }
                })?,
        );
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(watcher) = self.watcher.take()
            && self.signal.notify().is_ok()
        {
            let _ = watcher.join();
        }
    }
}

#[cfg(windows)]
mod os {
    use super::*;
    use std::{
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0},
        System::Threading::{CreateEventW, CreateMutexW, INFINITE, SetEvent, WaitForSingleObject},
        UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId,
        },
    };

    pub type Guard = OwnedHandle;
    pub struct Signal(OwnedHandle);

    fn own(handle: HANDLE) -> io::Result<OwnedHandle> {
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // These handles come from successful CreateEventW/CreateMutexW calls.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    pub fn acquire(_directory: &Path, id: u64) -> io::Result<(bool, Guard, Signal)> {
        let name = |suffix| {
            format!("Local\\codex-usage-widget-{id:016x}-{suffix}\0")
                .encode_utf16()
                .collect::<Vec<_>>()
        };
        let event_name = name("show");
        let mutex_name = name("owner");
        // Create the event before claiming ownership so cold-start notifications stay queued.
        // NUL-terminated names are valid for each call; handles are not inherited by children.
        let event = own(unsafe { CreateEventW(ptr::null(), 0, 0, event_name.as_ptr()) })?;
        let mutex = unsafe { CreateMutexW(ptr::null(), 0, mutex_name.as_ptr()) };
        let primary = unsafe { GetLastError() } != ERROR_ALREADY_EXISTS;
        Ok((primary, own(mutex)?, Signal(event)))
    }

    pub fn allow_activation() {
        let title: Vec<u16> = "Codex Usage Widget\0".encode_utf16().collect();
        // Only grant foreground permission to the existing widget's window.
        unsafe {
            let window = FindWindowW(ptr::null(), title.as_ptr());
            let mut pid = 0;
            if !window.is_null() && GetWindowThreadProcessId(window, &mut pid) != 0 {
                AllowSetForegroundWindow(pid);
            }
        }
    }

    impl Signal {
        pub fn notify(&self) -> io::Result<()> {
            // OwnedHandle keeps the event valid throughout the call.
            if unsafe { SetEvent(self.0.as_raw_handle()) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }

        pub fn wait(&self) -> io::Result<()> {
            // The listener holds an Arc until it has been woken and joined.
            if unsafe { WaitForSingleObject(self.0.as_raw_handle(), INFINITE) } == WAIT_OBJECT_0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }
}

#[cfg(unix)]
mod os {
    use super::*;
    use std::{
        fs::{self, File, OpenOptions, TryLockError},
        os::unix::{
            fs::{DirBuilderExt, PermissionsExt},
            net::UnixDatagram,
        },
        path::PathBuf,
    };

    pub type Guard = File;
    pub struct Signal {
        socket: UnixDatagram,
        path: PathBuf,
        primary: bool,
    }

    pub fn acquire(directory: &Path, id: u64) -> io::Result<(bool, Guard, Signal)> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
        let guard = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("instance.lock"))?;
        let primary = match guard.try_lock() {
            Ok(()) => true,
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Error(error)) => return Err(error),
        };
        // macOS supplies a per-user temporary directory; keep the socket pathname short.
        let path = std::env::temp_dir().join(format!("cuw-{id:016x}.sock"));
        let socket = if primary {
            // Holding the file lock proves any endpoint left by our previous run is stale.
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let socket = UnixDatagram::bind(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            socket
        } else {
            UnixDatagram::unbound()?
        };
        Ok((
            primary,
            guard,
            Signal {
                socket,
                path,
                primary,
            },
        ))
    }

    pub fn allow_activation() {}

    impl Signal {
        pub fn notify(&self) -> io::Result<()> {
            self.socket.send_to(&[1], &self.path).map(|_| ())
        }
        pub fn wait(&self) -> io::Result<()> {
            self.socket.recv(&mut [0u8; 1]).map(|_| ())
        }
    }

    impl Drop for Signal {
        fn drop(&mut self) {
            if self.primary {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn second_launch_notifies_owner_and_exit_releases_ownership() {
        let temporary = std::env::temp_dir();
        let root = temporary.join(format!(
            "codex-widget-instance-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut first = Instance::acquire_at(&root).unwrap().unwrap();
        let (tx, rx) = mpsc::channel();
        first
            .listen(move || {
                let _ = tx.send(());
            })
            .unwrap();
        assert!(Instance::acquire_at(&root).unwrap().is_none());
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(first);
        let replacement = Instance::acquire_at(&root).unwrap();
        assert!(replacement.is_some());
        drop(replacement);
        assert!(root.starts_with(&temporary) && root != temporary);
        if root.exists() {
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
