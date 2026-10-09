use crate::services::ServiceId;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub position: Option<[f32; 2]>,
    pub always_on_top: bool,
    pub bar_mode: bool,
    pub codex_path: Option<PathBuf>,
    /// Services to show, in order. One shows a single card; more are shown together.
    pub services: Vec<ServiceId>,
    /// Status line registrations made by this widget, kept so they can be undone.
    pub bridges: BTreeMap<ServiceId, Bridge>,
    /// No settings were saved yet, so the services in use still have to be found.
    #[serde(skip)]
    pub first_run: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            position: None,
            always_on_top: false,
            bar_mode: false,
            codex_path: None,
            // Settings saved before other services existed belong to Codex users.
            services: vec![ServiceId::Codex],
            bridges: BTreeMap::new(),
            first_run: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Bridge {
    /// The command this widget registered.
    pub command: String,
    /// The tool's own status line setting before registration, restored when unlinking.
    pub previous: Option<Value>,
}

impl Bridge {
    /// The user's own status line command, run so its output stays visible.
    pub fn previous_command(&self) -> Option<&str> {
        self.previous.as_ref()?.get("command")?.as_str()
    }
}

pub fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "codex-usage-widget")
}

/// Service icons the user provides, such as `claude.png`; the app does not bundle any logos.
pub fn icons_dir() -> Option<PathBuf> {
    project_dirs().map(|dirs| dirs.config_dir().join("icons"))
}

impl Settings {
    fn path() -> io::Result<PathBuf> {
        project_dirs()
            .map(|dirs| dirs.config_dir().join("settings.json"))
            .ok_or_else(|| io::Error::other("設定の保存先を取得できませんでした"))
    }

    pub fn load() -> Self {
        let Ok(bytes) = Self::path().and_then(fs::read) else {
            return Self {
                first_run: true,
                ..Self::default()
            };
        };
        let mut settings: Self = serde_json::from_slice(&bytes).unwrap_or_default();
        let mut seen = Vec::new();
        settings.services.retain(|service| {
            let new = !seen.contains(service);
            seen.push(*service);
            new
        });
        if settings.services.is_empty() {
            settings.services.push(ServiceId::Codex);
        }
        settings
    }

    pub fn save(&self) -> io::Result<()> {
        let path = Self::path()?;
        fs::create_dir_all(path.parent().expect("settings path has a parent"))?;
        write_atomic(&path, &serde_json::to_vec_pretty(self)?)
    }
}

/// Replace a file so readers never see it half written.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Several status line commands may write at once, so each uses its own temporary file.
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, bytes)?;
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
            let error = io::Error::last_os_error();
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_from_the_codex_only_version_keep_showing_codex() {
        let old: Settings = serde_json::from_value(json!({
            "position": [10.0, 20.0], "always_on_top": true, "bar_mode": false, "codex_path": null
        }))
        .unwrap();
        assert_eq!(old.services, [ServiceId::Codex]);
        assert!(old.bridges.is_empty());
        assert!(!old.first_run);
        let saved: Settings = serde_json::from_value(json!({
            "services": ["claude", "codex"],
            "bridges": {"claude": {"command": "widget statusline claude",
                "previous": {"type": "command", "command": "~/.claude/line.sh", "padding": 1}}}
        }))
        .unwrap();
        assert_eq!(saved.services, [ServiceId::ClaudeCode, ServiceId::Codex]);
        assert_eq!(
            saved.bridges[&ServiceId::ClaudeCode].previous_command(),
            Some("~/.claude/line.sh")
        );
        let round_trip: Settings =
            serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert_eq!(round_trip.bridges, saved.bridges);
    }

    #[test]
    fn atomic_writes_replace_existing_files() {
        let directory = std::env::temp_dir().join(format!(
            "widget-settings-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("data.json");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }
}
