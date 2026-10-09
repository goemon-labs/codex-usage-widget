//! Status line commands: a tool runs this widget with its session data while it is in use,
//! and the widget keeps only the usage numbers for its own display.
use crate::{
    process::ProcessGuard,
    services::{self, ServiceId, claude},
    settings::{self, Bridge, Settings},
};
use chrono::Local;
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

const MAX_INPUT: u64 = 1 << 20;
const MAX_OUTPUT: u64 = 1 << 16;
/// How long the user's own status line may run before its output is skipped.
const PREVIOUS_TIMEOUT: Duration = Duration::from_secs(10);

/// `statusline <service>`: store the usage numbers, then print what the status line shows.
pub fn run(key: &str) -> i32 {
    let Some(service) = ServiceId::from_key(key).filter(|service| service.received()) else {
        return 2;
    };
    let mut input = Vec::new();
    let _ = io::stdin().take(MAX_INPUT).read_to_end(&mut input);
    let data = serde_json::from_slice(&input)
        .ok()
        .and_then(|payload| services::extract(service, &payload));
    if let (Some(data), Some(path)) = (&data, received_path(service)) {
        let _ = store(&path, data, Local::now().timestamp());
    }
    // Keep the user's own status line visible; without one, show what is left.
    let previous = Settings::load()
        .bridges
        .get(&service)
        .and_then(Bridge::previous_command)
        .filter(|command| !is_widget_command(command, service))
        .map(String::from);
    let output = match previous {
        Some(command) => run_previous(&command, &input).unwrap_or_default(),
        None => summary(service, data.as_ref()).into_bytes(),
    };
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(&output);
    let _ = stdout.flush();
    0
}

pub fn received_path(service: ServiceId) -> Option<PathBuf> {
    let file = format!("{}.json", service.key());
    Some(
        settings::project_dirs()?
            .data_local_dir()
            .join("received")
            .join(file),
    )
}

/// The usage numbers a tool reported last, with the Unix time they arrived.
pub fn read(path: &Path) -> Option<(i64, Value)> {
    let value: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    Some((
        value.get("received_at")?.as_i64()?,
        value.get("data")?.clone(),
    ))
}

fn store(path: &Path, data: &Value, now: i64) -> io::Result<()> {
    // Status lines run often; rewrite only when the numbers change or a minute has passed.
    if let Some((received_at, stored)) = read(path)
        && stored == *data
        && (0..60).contains(&(now - received_at))
    {
        return Ok(());
    }
    fs::create_dir_all(path.parent().expect("received file has a parent"))?;
    let record = json!({"received_at": now, "data": data});
    settings::write_atomic(path, &serde_json::to_vec(&record)?)
}

fn summary(service: ServiceId, data: Option<&Value>) -> String {
    let now = Local::now().timestamp();
    let Some(snapshot) =
        data.and_then(|data| services::received_snapshot(service, data, Local::now(), now))
    else {
        return String::new();
    };
    let parts: Vec<_> = snapshot
        .groups
        .iter()
        .flat_map(|group| {
            group.windows.iter().map(move |window| {
                format!(
                    "{} {}",
                    group.window_label(window),
                    window.remaining_label()
                )
            })
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("残り {}", parts.join("・"))
    }
}

fn is_widget_command(command: &str, service: ServiceId) -> bool {
    command.contains(&format!("statusline {}", service.key()))
}

/// The settings file where a tool keeps its status line command.
pub fn tool_settings(service: ServiceId) -> Option<PathBuf> {
    match service {
        ServiceId::Codex => None,
        ServiceId::ClaudeCode => claude::settings_path(),
    }
}

/// The command a tool runs to hand over usage.
pub fn command(service: ServiceId) -> io::Result<String> {
    let executable = std::env::current_exe()?;
    Ok(format!(
        "{} statusline {}",
        shell_path(&executable),
        service.key()
    ))
}

/// Point the tool's status line at this widget, keeping what it showed before.
pub fn link(service: ServiceId, existing: Option<&Bridge>) -> Result<Bridge, String> {
    let path = tool_settings(service).ok_or("設定ファイルの場所を確認できませんでした。")?;
    let command = command(service).map_err(|_| "このアプリの場所を確認できませんでした。")?;
    let original = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return Err("設定ファイルを読み取れませんでした。".into()),
    };
    let mut value = match &original {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|_| UNREADABLE)?,
        None => json!({}),
    };
    let replaced = install(&mut value, &command)?;
    // Linking again keeps the setting from before the first link.
    let previous = match (replaced, existing) {
        (Some(line), Some(bridge)) if line_command(&line) == Some(bridge.command.as_str()) => {
            bridge.previous.clone()
        }
        (Some(line), _) if line_command(&line).is_some_and(|c| is_widget_command(c, service)) => {
            None
        }
        (replaced, _) => replaced,
    };
    write_tool_settings(service, &path, original.as_deref(), &value)?;
    Ok(Bridge { command, previous })
}

/// Restore the tool's previous status line. Returns false when the user had already changed it.
pub fn unlink(service: ServiceId, bridge: &Bridge) -> Result<bool, String> {
    let path = tool_settings(service).ok_or("設定ファイルの場所を確認できませんでした。")?;
    let original = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("設定ファイルを読み取れませんでした。".into()),
    };
    let mut value: Value = serde_json::from_slice(&original).map_err(|_| UNREADABLE)?;
    if !uninstall(&mut value, bridge) {
        return Ok(false);
    }
    write_tool_settings(service, &path, Some(&original), &value)?;
    Ok(true)
}

/// Keep a registration pointing at this executable after the app moves or is renamed.
pub fn relink(service: ServiceId, bridge: &Bridge) -> Option<Bridge> {
    let command = command(service).ok()?;
    if command == bridge.command {
        return None;
    }
    let path = tool_settings(service)?;
    let original = fs::read(&path).ok()?;
    let mut value: Value = serde_json::from_slice(&original).ok()?;
    let line = value.get_mut("statusLine")?.as_object_mut()?;
    if line.get("command")?.as_str()? != bridge.command {
        return None;
    }
    line.insert("command".into(), command.clone().into());
    write_tool_settings(service, &path, Some(&original), &value).ok()?;
    Some(Bridge {
        command,
        previous: bridge.previous.clone(),
    })
}

const UNREADABLE: &str = "設定ファイルの形式を確認できませんでした。手動で設定してください。";

fn line_command(line: &Value) -> Option<&str> {
    line.get("command")?.as_str()
}

/// Replace the status line command, returning the setting it replaced.
fn install(settings: &mut Value, command: &str) -> Result<Option<Value>, String> {
    let object = settings.as_object_mut().ok_or(UNREADABLE)?;
    let previous = object.get("statusLine").cloned();
    // Keep the user's spacing and refresh options; only the command changes.
    let mut line = previous
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    line.insert("type".into(), "command".into());
    line.insert("command".into(), command.into());
    object.insert("statusLine".into(), Value::Object(line));
    Ok(previous)
}

fn uninstall(settings: &mut Value, bridge: &Bridge) -> bool {
    let Some(object) = settings.as_object_mut() else {
        return false;
    };
    if object.get("statusLine").and_then(line_command) != Some(bridge.command.as_str()) {
        return false;
    }
    match &bridge.previous {
        Some(previous) => {
            object.insert("statusLine".into(), previous.clone());
        }
        None => {
            object.shift_remove("statusLine");
        }
    }
    true
}

fn write_tool_settings(
    service: ServiceId,
    path: &Path,
    original: Option<&[u8]>,
    value: &Value,
) -> Result<(), String> {
    let failed = |_| "設定ファイルを書き込めませんでした。".to_string();
    // Keep a copy of the user's file in this app's data folder before changing it.
    if let Some(original) = original
        && let Some(dirs) = settings::project_dirs()
    {
        let backups = dirs.data_local_dir().join("backups");
        let name = format!(
            "{}-settings-{}.json",
            service.key(),
            Local::now().format("%Y%m%d-%H%M%S")
        );
        fs::create_dir_all(&backups).map_err(failed)?;
        fs::write(backups.join(name), original).map_err(failed)?;
    }
    fs::create_dir_all(
        path.parent()
            .ok_or("設定ファイルの場所を確認できませんでした。")?,
    )
    .map_err(failed)?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| UNREADABLE)?;
    settings::write_atomic(path, &bytes).map_err(failed)
}

/// Run the user's previous status line with the same input and return what it printed.
fn run_previous(command: &str, input: &[u8]) -> Option<Vec<u8>> {
    let mut shell = shell(command);
    shell
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        shell.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        shell.process_group(0);
    }
    let mut process = ProcessGuard::new(shell.spawn().ok()?).ok()?;
    let mut stdin = process.child.stdin.take()?;
    let stdout = process.child.stdout.take()?;
    let input = input.to_vec();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.take(MAX_OUTPUT).read_to_end(&mut output);
        let _ = sender.send(output);
    });
    let output = receiver.recv_timeout(PREVIOUS_TIMEOUT).ok();
    // Ending the command also ends anything it started and releases both pipes.
    drop(process);
    let _ = writer.join();
    output
}

#[cfg(windows)]
fn shell(command: &str) -> Command {
    // Claude Code runs status lines through Git Bash when it is installed, otherwise PowerShell.
    match git_bash() {
        Some(bash) => {
            let mut shell = Command::new(bash);
            shell.arg("-c").arg(command);
            shell
        }
        None => {
            let mut shell = Command::new("powershell.exe");
            shell.args(["-NoProfile", "-NonInteractive", "-Command", command]);
            shell
        }
    }
}

#[cfg(not(windows))]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("/bin/sh");
    shell.arg("-c").arg(command);
    shell
}

#[cfg(windows)]
fn git_bash() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(path);
    }
    // git.exe lives in Git\cmd or Git\mingw64\bin; Git Bash is Git\bin\bash.exe.
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|directory| {
        if !directory.join("git.exe").is_file() {
            return None;
        }
        directory
            .ancestors()
            .take(3)
            .map(|root| root.join("bin").join("bash.exe"))
            .find(|path| path.is_file())
    })
}

/// Write a path so the tool's shell runs it as a command with arguments.
#[cfg(windows)]
fn shell_path(path: &Path) -> String {
    let text = |path: &Path| path.to_string_lossy().replace('\\', "/");
    let long = text(path);
    if !long.contains(' ') {
        return long;
    }
    // Git Bash drops backslashes and PowerShell cannot run a quoted path with arguments,
    // so prefer the short form of a path with spaces and quote only when there is none.
    match short_path(path)
        .map(|short| text(&short))
        .filter(|short| !short.contains(' '))
    {
        Some(short) => short,
        None => format!("\"{long}\""),
    }
}

#[cfg(windows)]
fn short_path(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // The first call reports the size; the second fills a buffer of exactly that size.
    unsafe {
        let length = GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0);
        if length == 0 {
            return None;
        }
        let mut buffer = vec![0u16; length as usize];
        let written = GetShortPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), length);
        if written == 0 || written >= length {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsString::from_wide(
            &buffer[..written as usize],
        )))
    }
}

#[cfg(not(windows))]
fn shell_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        text.into_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "widget-bridge-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn linking_keeps_other_settings_and_unlinking_restores_the_previous_line() {
        let original = json!({
            "model": "opus",
            "statusLine": {"type": "command", "command": "~/.claude/line.sh", "padding": 2},
            "theme": "dark"
        });
        let mut settings = original.clone();
        let previous = install(&mut settings, "widget statusline claude").unwrap();
        assert_eq!(previous, Some(original["statusLine"].clone()));
        assert_eq!(
            settings["statusLine"],
            json!({"type": "command", "command": "widget statusline claude", "padding": 2})
        );
        let bridge = Bridge {
            command: "widget statusline claude".into(),
            previous,
        };
        assert!(uninstall(&mut settings, &bridge));
        assert_eq!(settings, original);
        assert_eq!(
            serde_json::to_string(&settings).unwrap(),
            serde_json::to_string(&original).unwrap()
        );
    }

    #[test]
    fn unlinking_without_a_previous_line_removes_it_and_respects_user_changes() {
        let mut settings = json!({"a": 1, "b": 2});
        let previous = install(&mut settings, "widget statusline claude").unwrap();
        assert!(previous.is_none());
        let bridge = Bridge {
            command: "widget statusline claude".into(),
            previous,
        };
        let mut changed = settings.clone();
        changed["statusLine"]["command"] = "~/mine.sh".into();
        let before = changed.clone();
        assert!(!uninstall(&mut changed, &bridge));
        assert_eq!(changed, before);
        assert!(uninstall(&mut settings, &bridge));
        assert_eq!(
            serde_json::to_string(&settings).unwrap(),
            r#"{"a":1,"b":2}"#
        );
        assert!(install(&mut json!([]), "x").is_err());
    }

    #[test]
    fn received_numbers_are_rewritten_only_when_they_change_or_age() {
        let directory = temporary("store");
        let path = directory.join("received").join("claude.json");
        let first = json!({"five_hour": {"used_percentage": 10}});
        store(&path, &first, 1_000).unwrap();
        assert_eq!(read(&path), Some((1_000, first.clone())));
        store(&path, &first, 1_030).unwrap();
        assert_eq!(read(&path).unwrap().0, 1_000);
        store(&path, &first, 1_061).unwrap();
        assert_eq!(read(&path).unwrap().0, 1_061);
        let second = json!({"five_hour": {"used_percentage": 11}});
        store(&path, &second, 1_062).unwrap();
        assert_eq!(read(&path), Some((1_062, second)));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_status_line_summary_lists_what_is_left() {
        let data = json!({
            "five_hour": {"used_percentage": 29},
            "seven_day": {"used_percentage": 68.4}
        });
        assert_eq!(
            summary(ServiceId::ClaudeCode, Some(&data)),
            "残り 5時間 71%・週次 31%"
        );
        assert_eq!(summary(ServiceId::ClaudeCode, None), "");
        assert!(is_widget_command(
            "C:/app/widget.exe statusline claude",
            ServiceId::ClaudeCode
        ));
        assert!(!is_widget_command(
            "~/.claude/line.sh",
            ServiceId::ClaudeCode
        ));
    }

    #[cfg(windows)]
    #[test]
    fn registered_paths_have_no_backslashes_or_bare_spaces() {
        let directory = temporary("path with space");
        let executable = directory.join("app.exe");
        fs::write(&executable, b"").unwrap();
        let path = shell_path(&executable);
        assert!(!path.contains('\\'), "{path}");
        assert!(!path.contains(' ') || path.starts_with('"'), "{path}");
        assert_eq!(
            shell_path(Path::new(r"C:\Apps\widget.exe")),
            "C:/Apps/widget.exe"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
