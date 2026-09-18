use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{fs, io, path::PathBuf};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub position: Option<[f32; 2]>,
    pub always_on_top: bool,
    pub bar_mode: bool,
    pub codex_path: Option<PathBuf>,
}

impl Settings {
    fn path() -> io::Result<PathBuf> {
        ProjectDirs::from("", "", "codex-usage-widget")
            .map(|dirs| dirs.config_dir().join("settings.json"))
            .ok_or_else(|| io::Error::other("設定の保存先を取得できませんでした"))
    }

    pub fn load() -> Self {
        Self::path()
            .ok()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = Self::path()?;
        fs::create_dir_all(path.parent().expect("settings path has a parent"))?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        // Windows rename cannot replace an existing file. MoveFileExW can.
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            };
            let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            // Both buffers are NUL terminated and live for the duration of the call.
            if unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(windows))]
        fs::rename(temporary, path)
    }
}
