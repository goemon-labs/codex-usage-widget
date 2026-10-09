use std::path::{Path, PathBuf};

/// Locate the desktop app independently of PATH and standalone CLI installations.
pub fn find() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let bundled = windows::bundled()?;
        Some(windows::existing_runtime(&bundled).unwrap_or(bundled))
    }

    #[cfg(target_os = "macos")]
    {
        let mut directories = vec![PathBuf::from("/Applications")];
        if let Some(dirs) = directories::BaseDirs::new() {
            directories.push(dirs.home_dir().join("Applications"));
        }
        directories.into_iter().find_map(|directory| {
            ["Codex.app", "ChatGPT.app"].into_iter().find_map(|name| {
                let path = directory.join(name).join("Contents/Resources/codex");
                path.is_file().then_some(path)
            })
        })
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    None
}

/// Resolve an app selected in the file picker to its existing Codex executable.
pub fn from_path(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let bundled = windows::bundled()?;
        let app_directory = bundled.parent()?.parent()?;
        let app_selected = path == bundled
            || (path.parent() == Some(app_directory)
                && path.file_name().is_some_and(|name| {
                    name.eq_ignore_ascii_case("Codex.exe")
                        || name.eq_ignore_ascii_case("ChatGPT.exe")
                }));
        app_selected.then(|| windows::existing_runtime(&bundled).unwrap_or(bundled))
    }
    #[cfg(target_os = "macos")]
    {
        let executable = path.join("Contents/Resources/codex");
        executable.is_file().then_some(executable)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = path;
        None
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        ffi::OsString,
        fs,
        io::{self, Read},
        os::windows::ffi::OsStringExt,
        ptr,
        sync::Mutex,
        time::SystemTime,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
        Storage::Packaging::Appx::{GetPackagePathByFullName, GetPackagesByPackageFamily},
    };

    pub(super) fn bundled() -> Option<PathBuf> {
        let family: Vec<u16> = "OpenAI.Codex_2p2nqsd0c76g0\0".encode_utf16().collect();
        let (mut count, mut length) = (0, 0);
        // Query the current user's registered package, including installations on other drives.
        // All output buffers stay alive until their returned pointers have been consumed.
        unsafe {
            if GetPackagesByPackageFamily(
                family.as_ptr(),
                &mut count,
                ptr::null_mut(),
                &mut length,
                ptr::null_mut(),
            ) != ERROR_INSUFFICIENT_BUFFER
            {
                return None;
            }
            let mut names = vec![ptr::null_mut(); count as usize];
            let mut buffer = vec![0u16; length as usize];
            if GetPackagesByPackageFamily(
                family.as_ptr(),
                &mut count,
                names.as_mut_ptr(),
                &mut length,
                buffer.as_mut_ptr(),
            ) != ERROR_SUCCESS
            {
                return None;
            }
            for name in names.into_iter().take(count as usize) {
                let mut length = 0;
                if GetPackagePathByFullName(name, &mut length, ptr::null_mut())
                    != ERROR_INSUFFICIENT_BUFFER
                {
                    continue;
                }
                let mut path = vec![0u16; length as usize];
                if GetPackagePathByFullName(name, &mut length, path.as_mut_ptr()) != ERROR_SUCCESS {
                    continue;
                }
                let end = path.iter().position(|&c| c == 0)?;
                let executable = PathBuf::from(OsString::from_wide(&path[..end]))
                    .join("app/resources/codex.exe");
                if executable.is_file() {
                    return Some(executable);
                }
            }
        }
        None
    }

    pub(super) fn existing_runtime(bundled: &Path) -> Option<PathBuf> {
        let directory = directories::BaseDirs::new()?
            .data_local_dir()
            .join("OpenAI/Codex/bin");
        matching_runtime(bundled, &directory)
    }

    #[derive(PartialEq)]
    struct FileStamp {
        path: PathBuf,
        length: u64,
        modified: SystemTime,
    }

    impl FileStamp {
        fn read(path: &Path) -> Option<Self> {
            let metadata = fs::metadata(path).ok()?;
            metadata.is_file().then_some(Self {
                path: path.to_path_buf(),
                length: metadata.len(),
                modified: metadata.modified().ok()?,
            })
        }
    }

    struct VerifiedRuntime {
        directory: PathBuf,
        bundled: FileStamp,
        runtime: FileStamp,
    }

    static VERIFIED: Mutex<Option<VerifiedRuntime>> = Mutex::new(None);

    fn matching_runtime(bundled: &Path, directory: &Path) -> Option<PathBuf> {
        let source = FileStamp::read(bundled)?;
        if let Ok(verified) = VERIFIED.lock()
            && let Some(verified) = verified.as_ref()
            && verified.directory == directory
            && verified.bundled == source
            && FileStamp::read(&verified.runtime.path).as_ref() == Some(&verified.runtime)
        {
            return Some(verified.runtime.path.clone());
        }
        let mut paths: Vec<_> = fs::read_dir(directory)
            .ok()?
            .flatten()
            .map(|entry| entry.path().join("codex.exe"))
            .collect();
        paths.sort();
        for path in paths {
            let Some(runtime) = FileStamp::read(&path) else {
                continue;
            };
            if runtime.length == source.length && same_contents(bundled, &path).unwrap_or(false) {
                if let Ok(mut verified) = VERIFIED.lock() {
                    *verified = Some(VerifiedRuntime {
                        directory: directory.to_path_buf(),
                        bundled: source,
                        runtime,
                    });
                }
                return Some(path);
            }
        }
        None
    }

    fn same_contents(bundled: &Path, candidate: &Path) -> io::Result<bool> {
        let mut source = fs::File::open(bundled)?;
        let mut candidate = fs::File::open(candidate)?;
        // Compare in small blocks; neither executable is copied or loaded into memory in full.
        let (mut left, mut right) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
        loop {
            let length = source.read(&mut left)?;
            if length == 0 {
                return Ok(true);
            }
            candidate.read_exact(&mut right[..length])?;
            if left[..length] != right[..length] {
                return Ok(false);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn detection_requires_matching_contents_and_rechecks_app_updates() {
            let temporary = std::env::temp_dir();
            let root = temporary.join(format!(
                "codex-widget-detection-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            ));
            let bundled = root.join("app.exe");
            let runtimes = root.join("runtimes");
            fs::create_dir(&root).unwrap();
            fs::write(&bundled, b"version-one").unwrap();
            assert!(matching_runtime(&bundled, &runtimes).is_none());
            assert!(!runtimes.exists());
            let correct = runtimes.join("older/codex.exe");
            let unrelated = runtimes.join("newer/codex.exe");
            for (path, content, seconds) in [
                (&correct, b"version-one", 1_700_000_000),
                (&unrelated, b"wrong-build", 1_700_000_100),
            ] {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, content).unwrap();
                let file = fs::OpenOptions::new().write(true).open(path).unwrap();
                file.set_times(
                    fs::FileTimes::new().set_modified(
                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
                    ),
                )
                .unwrap();
            }
            assert_eq!(matching_runtime(&bundled, &runtimes).unwrap(), correct);
            assert_eq!(matching_runtime(&bundled, &runtimes).unwrap(), correct);
            fs::write(&correct, b"wrong-again").unwrap();
            assert!(matching_runtime(&bundled, &runtimes).is_none());
            // Same-size app replacement must invalidate the in-memory match.
            fs::write(&bundled, b"wrong-build").unwrap();
            assert_eq!(matching_runtime(&bundled, &runtimes).unwrap(), unrelated);
            assert!(root.starts_with(&temporary) && root != temporary);
            fs::remove_dir_all(root).unwrap();
        }
    }
}
