use crate::quota::Usage;
use serde_json::{Value, json};
use std::{
    env,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(30);
pub const SETUP_GUIDANCE: &str = "Codexが見つかりません。CLIまたはアプリをインストールしてください。インストール済みの場合は、設定の「場所を選択」で指定してください。";

#[derive(Clone, Debug)]
pub struct Installation {
    pub path: PathBuf,
    pub is_app: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub email: Option<String>,
    pub plan: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ErrorKind {
    Setup,
    Login,
    Connection,
    Unsupported,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct FetchError {
    pub kind: ErrorKind,
    pub message: String,
}

impl FetchError {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    fn connection() -> Self {
        Self::new(
            ErrorKind::Connection,
            "Codexに接続できませんでした。通信状態を確認して再試行してください。",
        )
    }
}

pub fn find_app() -> Option<Installation> {
    crate::codex_app::find().map(|path| Installation { path, is_app: true })
}

pub fn find_codex(configured: Option<&Path>) -> Option<Installation> {
    if let Some(path) = configured {
        if let Some(path) = crate::codex_app::from_path(path) {
            return Some(Installation { path, is_app: true });
        }
        let path = native_path(path)?;
        let is_app = crate::codex_app::find().is_some_and(|app| app == path);
        return Some(Installation { path, is_app });
    }
    let mut paths = Vec::new();
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            paths.push(directory.join(if cfg!(windows) { "codex.exe" } else { "codex" }));
            #[cfg(windows)]
            paths.push(directory.join("codex.cmd"));
        }
    }
    #[cfg(windows)]
    {
        if let Some(local) = env::var_os("LOCALAPPDATA") {
            paths.push(PathBuf::from(local).join("Programs/OpenAI/Codex/bin/codex.exe"));
        }
        if let Some(roaming) = env::var_os("APPDATA") {
            paths.push(PathBuf::from(roaming).join("npm/codex.cmd"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        paths.extend([
            PathBuf::from("/opt/homebrew/bin/codex"),
            PathBuf::from("/usr/local/bin/codex"),
        ]);
        if let Some(dirs) = directories::BaseDirs::new() {
            paths.push(dirs.home_dir().join(".local/bin/codex"));
        }
    }
    paths
        .into_iter()
        .find_map(|path| native_path(&path))
        .map(|path| Installation {
            path,
            is_app: false,
        })
        .or_else(find_app)
}

fn native_path(path: &Path) -> Option<PathBuf> {
    if !path.is_file() {
        return None;
    }
    #[cfg(windows)]
    {
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"))
        {
            let target = if cfg!(target_arch = "aarch64") {
                "aarch64-pc-windows-msvc"
            } else {
                "x86_64-pc-windows-msvc"
            };
            let package = path.parent()?.join("node_modules/@openai/codex");
            let relative = PathBuf::from("vendor").join(target).join("codex/codex.exe");
            let legacy = package.join(&relative);
            if legacy.is_file() {
                return Some(legacy);
            }
            for scope in [
                package.join("node_modules/@openai"),
                package.parent()?.to_path_buf(),
            ] {
                for entry in std::fs::read_dir(scope)
                    .ok()
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    if entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("codex-win32-")
                    {
                        let candidate = entry.path().join(&relative);
                        if candidate.is_file() {
                            return Some(candidate);
                        }
                    }
                }
            }
            return None;
        }
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            return None;
        }
    }
    Some(path.to_path_buf())
}

pub fn fetch(
    installation: &Installation,
    cancel: &Arc<AtomicBool>,
    account_changed: impl FnOnce(Account),
) -> Result<Usage, FetchError> {
    let path = &installation.path;
    if cancel.load(Ordering::Relaxed) {
        return Err(FetchError::new(ErrorKind::Cancelled, "取得を終了しました"));
    }
    let mut command = Command::new(path);
    command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Run outside a project so this read-only helper does not select project configuration.
    if let Some(dirs) = directories::BaseDirs::new() {
        command.current_dir(dirs.home_dir());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        // Finder-launched apps have a smaller PATH than a terminal.
        let mut bins: Vec<PathBuf> =
            env::split_paths(&env::var_os("PATH").unwrap_or_default()).collect();
        if let Some(parent) = path.parent() {
            bins.push(parent.to_path_buf());
        }
        bins.extend([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]);
        if let Ok(path) = env::join_paths(bins) {
            command.env("PATH", path);
        }
    }
    let child = command.spawn().map_err(|_| {
        FetchError::new(
            ErrorKind::Setup,
            "Codexを起動できませんでした。設定の「場所を選択」で、利用可能なCodexの実行ファイルを指定してください。",
        )
    })?;
    let mut process = ProcessGuard::new(child)?;
    let stdout = process
        .child
        .stdout
        .take()
        .ok_or_else(FetchError::connection)?;
    let mut stdin = process
        .child
        .stdin
        .take()
        .ok_or_else(FetchError::connection)?;
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            let Ok(value) = serde_json::from_str(&line) else {
                continue;
            };
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + TIMEOUT;
    let result = (|| {
        send(
            &mut stdin,
            json!({"id": 1, "method": "initialize", "params": {
                "clientInfo": {"name": "codex_usage_widget", "title": "Codex Usage Widget", "version": env!("CARGO_PKG_VERSION")}
            }}),
        )?;
        receive(&rx, 1, deadline, cancel)?;
        send(&mut stdin, json!({"method": "initialized", "params": {}}))?;
        send(
            &mut stdin,
            json!({"id": 2, "method": "account/read", "params": {"refreshToken": false}}),
        )?;
        let result = receive(&rx, 2, deadline, cancel)?;
        let account = &result["account"];
        if account["type"].as_str() != Some("chatgpt") {
            return Err(FetchError::new(
                ErrorKind::Login,
                "公式CodexにChatGPTアカウントでログインしてください。",
            ));
        }
        account_changed(Account {
            email: account["email"].as_str().map(String::from),
            plan: account["planType"].as_str().map(String::from),
        });
        send(
            &mut stdin,
            json!({"id": 3, "method": "account/rateLimits/read"}),
        )?;
        let response = receive(&rx, 3, deadline, cancel)?;
        Usage::from_response(response)
            .map_err(|message| FetchError::new(ErrorKind::Unsupported, message))
    })();
    drop(stdin);
    drop(process); // Reap the helper and its descendants on success, failure, and cancellation.
    let _ = reader.join();
    result
}

fn send(stdin: &mut ChildStdin, value: Value) -> Result<(), FetchError> {
    writeln!(stdin, "{value}")
        .and_then(|()| stdin.flush())
        .map_err(|_| FetchError::connection())
}

fn receive(
    rx: &Receiver<Value>,
    id: u64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Value, FetchError> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(FetchError::new(ErrorKind::Cancelled, "取得を終了しました"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(FetchError::new(
                ErrorKind::Connection,
                "取得に時間がかかっています。しばらくしてから再試行してください。",
            ));
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(value) if value["id"].as_u64() == Some(id) => {
                if let Some(error) = value.get("error") {
                    // Classify locally; raw server errors may contain account or configuration data.
                    let code = error["code"].as_i64();
                    let message = error["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    let login = matches!(code, Some(401 | 403))
                        || message.contains("unauthorized")
                        || message.contains("not authenticated")
                        || message.contains("not logged in")
                        || message.contains("token expired")
                        || message.contains("token has expired");
                    return Err(if login {
                        FetchError::new(
                            ErrorKind::Login,
                            "公式Codexでログインを確認し、再試行してください。",
                        )
                    } else if code == Some(-32601) {
                        FetchError::new(
                            ErrorKind::Unsupported,
                            "このCodexは利用枠の取得に対応していません。Codexを更新してください。",
                        )
                    } else {
                        FetchError::connection()
                    });
                }
                return value
                    .get("result")
                    .cloned()
                    .ok_or_else(FetchError::connection);
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(FetchError::connection()),
        }
    }
}

struct ProcessGuard {
    child: Child,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

impl ProcessGuard {
    fn new(child: Child) -> Result<Self, FetchError> {
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
                    if !job.is_null() {
                        CloseHandle(job);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(FetchError::new(
                        ErrorKind::Setup,
                        "Codexの補助処理を開始できませんでした。",
                    ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_ignores_notifications_and_classifies_login_errors() {
        let (tx, rx) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        tx.send(json!({"method": "account/updated", "params": {}}))
            .unwrap();
        tx.send(json!({"id": 3, "result": {"ok": true}})).unwrap();
        assert_eq!(
            receive(&rx, 3, Instant::now() + TIMEOUT, &cancel).unwrap(),
            json!({"ok": true})
        );
        tx.send(json!({"id": 4, "error": {"code": 401, "message": "private detail"}}))
            .unwrap();
        let error = receive(&rx, 4, Instant::now() + TIMEOUT, &cancel).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Login);
        assert!(!error.message.contains("private detail"));
    }

    #[test]
    fn rpc_stops_on_timeout_cancellation_and_closed_pipe() {
        let (tx, rx) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            receive(&rx, 1, Instant::now(), &cancel).unwrap_err().kind,
            ErrorKind::Connection
        );
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            receive(&rx, 1, Instant::now() + TIMEOUT, &cancel)
                .unwrap_err()
                .kind,
            ErrorKind::Cancelled
        );
        cancel.store(false, Ordering::Relaxed);
        drop(tx);
        assert_eq!(
            receive(&rx, 1, Instant::now() + TIMEOUT, &cancel)
                .unwrap_err()
                .kind,
            ErrorKind::Connection
        );
    }
}
