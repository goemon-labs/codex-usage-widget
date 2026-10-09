use crate::{
    process::ProcessGuard,
    quota::{self, Blocked, Cap, Group, ResetCredits, Snapshot, Span, Window},
};
use chrono::Local;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    env,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
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
    super::codex_app::find().map(|path| Installation { path })
}

pub fn find_codex(configured: Option<&Path>) -> Option<Installation> {
    if let Some(path) = configured {
        let path = super::codex_app::from_path(path).or_else(|| native_path(path))?;
        return Some(Installation { path });
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
        .map(|path| Installation { path })
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
) -> Result<Snapshot, FetchError> {
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
    let mut process = ProcessGuard::new(child).map_err(|_| {
        FetchError::new(ErrorKind::Setup, "Codexの補助処理を開始できませんでした。")
    })?;
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
        snapshot(&response, Local::now().timestamp())
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

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawSnapshot {
    limit_id: Option<String>,
    limit_name: Option<String>,
    primary: Option<RawWindow>,
    secondary: Option<RawWindow>,
    credits: Option<RawCredits>,
    individual_limit: Option<RawIndividualLimit>,
    spend_control_reached: Option<bool>,
    rate_limit_reached_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWindow {
    used_percent: Option<f64>,
    window_duration_mins: Option<u64>,
    resets_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCredits {
    has_credits: bool,
    unlimited: bool,
    balance: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawIndividualLimit {
    limit: String,
    used: String,
    remaining_percent: f64,
    resets_at: i64,
}

impl RawWindow {
    fn normalize(self) -> Option<Window> {
        let window = Window {
            span: self
                .window_duration_mins
                .filter(|value| *value > 0)
                .map_or(Span::Unknown, Span::Minutes),
            remaining: self
                .used_percent
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(|used| (100.0 - used).clamp(0.0, 100.0)),
            resets_at: self.resets_at.filter(|value| *value > 0),
        };
        // A window of unknown length is still worth showing when it reports what is left.
        (window.span != Span::Unknown || window.remaining.is_some()).then_some(window)
    }
}

impl RawSnapshot {
    fn windows(&mut self) -> Vec<Window> {
        [self.primary.take(), self.secondary.take()]
            .into_iter()
            .flatten()
            .filter_map(RawWindow::normalize)
            .collect()
    }
}

/// Credits arrive as decimal text in Codex's own unit; like Codex, show whole credits.
fn credits(raw: &str) -> String {
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map_or_else(|| raw.to_string(), quota::amount_label)
}

/// Convert an `account/rateLimits/read` response into display data.
fn snapshot(response: &Value, now: i64) -> Result<Snapshot, &'static str> {
    let buckets = response
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object);
    let main = match buckets {
        Some(buckets) => buckets
            .get("codex")
            .ok_or("Codexの利用枠を確認できませんでした")?,
        None => response
            .get("rateLimits")
            .filter(|value| !value.is_null())
            .ok_or("利用枠の応答を確認できませんでした")?,
    };
    let mut main: RawSnapshot =
        serde_json::from_value(main.clone()).map_err(|_| "利用枠の形式を確認できませんでした")?;
    if main.limit_id.as_deref().is_some_and(|id| id != "codex") {
        return Err("Codexの利用枠を確認できませんでした");
    }
    let mut groups = vec![Group::new(None, main.windows())];
    // Additional metered limits, such as model-specific allowances, are named by the server.
    for (id, bucket) in buckets
        .into_iter()
        .flatten()
        .filter(|(id, _)| *id != "codex")
    {
        let Ok(mut bucket) = serde_json::from_value::<RawSnapshot>(bucket.clone()) else {
            continue;
        };
        let windows = bucket.windows();
        if !windows.is_empty() {
            let name = bucket.limit_name.unwrap_or_else(|| id.clone());
            groups.push(Group::new(Some(name), windows));
        }
    }
    let cap = main.individual_limit.map(|limit| Cap {
        label: "月間クレジット上限".into(),
        remaining: Some(limit.remaining_percent)
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.0, 100.0)),
        detail: Some(format!(
            "{} / {} クレジット使用",
            credits(&limit.used),
            credits(&limit.limit)
        )),
        resets_at: Some(limit.resets_at).filter(|value| *value > 0),
    });
    let balance = main.credits.and_then(|account| {
        if account.unlimited {
            Some("無制限".into())
        } else if account.has_credits {
            account.balance.as_deref().map(credits)
        } else {
            None
        }
    });
    // The server decides whether included usage may continue; percentages cannot prove it.
    let allowed = response
        .get("ordinaryUsageAllowed")
        .and_then(Value::as_bool);
    let spend_reached = main.spend_control_reached == Some(true);
    let reached = allowed == Some(false) || main.rate_limit_reached_type.is_some() || spend_reached;
    let blocked = if allowed == Some(true) && !reached {
        None
    } else {
        quota::blocked(&groups[..1], now).or_else(|| {
            reached.then(|| Blocked {
                label: if spend_reached {
                    "月間クレジット上限".into()
                } else {
                    "利用上限".into()
                },
                until: cap
                    .as_ref()
                    .filter(|_| spend_reached)
                    .and_then(|cap| cap.resets_at),
                minutes: None,
            })
        })
    };
    Ok(Snapshot {
        groups,
        cap,
        balance,
        reset_credits: response
            .get("rateLimitResetCredits")
            .and_then(ResetCredits::from_response),
        blocked,
        observed_at: Local::now(),
    })
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

    #[test]
    fn identifies_week_by_duration_and_prefers_codex_bucket() {
        let usage = snapshot(
            &json!({
                "rateLimits": {"primary": {"windowDurationMins": 10080, "usedPercent": 99}},
                "rateLimitsByLimitId": {"codex": {
                    "primary": {"windowDurationMins": 10080, "usedPercent": 81},
                    "secondary": {"windowDurationMins": 300, "usedPercent": 12}
                }}
            }),
            0,
        )
        .unwrap();
        assert_eq!(usage.main_window().unwrap().remaining_label(), "19%");
        assert_eq!(usage.groups[0].windows[0].label(), "5時間");
        assert!(snapshot(&json!({"rateLimitsByLimitId": {"other": {}}}), 0).is_err());
    }

    #[test]
    fn legacy_missing_values_and_rounding_do_not_invent_remaining_quota() {
        for (used, expected) in [
            (0.0, "100%"),
            (100.0, "0%"),
            (101.0, "0%"),
            (99.7, "1%未満"),
            (-1.0, "—"),
        ] {
            let usage = snapshot(
                &json!({"rateLimits": {
                    "secondary": {"windowDurationMins": 10080, "usedPercent": used}
                }}),
                0,
            )
            .unwrap();
            let week = usage.main_window().unwrap();
            assert_eq!(week.remaining_label(), expected);
            assert_eq!(week.resets_at, None);
        }
        let usage = snapshot(&json!({"rateLimits": {"primary": null}}), 0).unwrap();
        assert!(usage.main_window().is_none());
        assert!(usage.blocked.is_none());
        // A window without a length is kept only when it says how much is left.
        let usage = snapshot(
            &json!({"rateLimits": {
                "primary": {"usedPercent": 30}, "secondary": {"resetsAt": 5}
            }}),
            0,
        )
        .unwrap();
        let labels: Vec<_> = usage.groups[0].windows.iter().map(Window::label).collect();
        assert_eq!(labels, ["利用枠"]);
        assert_eq!(usage.main_window().unwrap().remaining_label(), "70%");
    }

    #[test]
    fn weekly_only_monthly_and_named_extra_limits_keep_every_window() {
        let usage = snapshot(
            &json!({"rateLimitsByLimitId": {
                "codex": {"limitId": "codex",
                    "primary": {"windowDurationMins": 10079, "usedPercent": 40},
                    "secondary": {"windowDurationMins": 43200, "usedPercent": 10}},
                "codex_other": {"limitId": "codex_other", "limitName": "Codex-Spark",
                    "primary": {"windowDurationMins": 300, "usedPercent": 30}},
                "empty": {"limitId": "empty"},
                "broken": {"primary": "unexpected"}
            }}),
            0,
        )
        .unwrap();
        let labels: Vec<_> = usage.groups[0].windows.iter().map(Window::label).collect();
        assert_eq!(labels, ["週次", "月次"]);
        assert_eq!(usage.main_window().unwrap().remaining_label(), "60%");
        assert_eq!(usage.groups.len(), 2);
        let extra = &usage.groups[1];
        assert_eq!(extra.window_label(&extra.windows[0]), "Codex-Spark・5時間");
    }

    #[test]
    fn credit_plans_show_the_monthly_cap_and_balance_without_time_windows() {
        let usage = snapshot(
            &json!({"rateLimits": {
                "limitId": "codex",
                "credits": {"hasCredits": true, "unlimited": false, "balance": "62500.3712345678"},
                "individualLimit": {"limit": "500", "used": "314",
                    "remainingPercent": 37.2, "resetsAt": 1_800_000_000}
            }}),
            0,
        )
        .unwrap();
        assert!(usage.main_window().is_none());
        assert!(usage.reset_credits.is_none());
        assert_eq!(usage.balance.as_deref(), Some("62,500"));
        let cap = usage.cap.unwrap();
        assert_eq!(cap.label, "月間クレジット上限");
        assert_eq!(cap.detail.as_deref(), Some("314 / 500 クレジット使用"));
        assert_eq!(cap.resets_at, Some(1_800_000_000));
        let balance = |credits: Value| {
            snapshot(&json!({"rateLimits": {"credits": credits}}), 0)
                .unwrap()
                .balance
        };
        assert_eq!(
            balance(json!({"hasCredits": false, "unlimited": true, "balance": null})).as_deref(),
            Some("無制限")
        );
        assert!(
            balance(json!({"hasCredits": false, "unlimited": false, "balance": "0"})).is_none()
        );
    }

    #[test]
    fn the_server_decides_when_included_usage_is_blocked() {
        let response = |allowed: Value, used: f64| {
            json!({
                "ordinaryUsageAllowed": allowed,
                "rateLimits": {
                    "primary": {"windowDurationMins": 300, "usedPercent": used, "resetsAt": 5_000},
                    "secondary": {"windowDurationMins": 10080, "usedPercent": 20, "resetsAt": 9_000}
                }
            })
        };
        let blocked = |allowed, used| snapshot(&response(allowed, used), 1_000).unwrap().blocked;
        assert_eq!(
            blocked(json!(false), 100.0),
            Some(Blocked {
                label: "5時間の枠".into(),
                until: Some(5_000),
                minutes: Some(300),
            })
        );
        // Credits can keep usage going even when a window is used up.
        assert!(blocked(json!(true), 100.0).is_none());
        // Older Codex versions omit the flag, so an exhausted window decides.
        assert!(blocked(Value::Null, 100.0).is_some());
        assert!(blocked(Value::Null, 50.0).is_none());
        let unknown = blocked(json!(false), 50.0).unwrap();
        assert_eq!((unknown.label.as_str(), unknown.until), ("利用上限", None));
        let spend = snapshot(
            &json!({"rateLimits": {"spendControlReached": true, "individualLimit":
                {"limit": "500", "used": "500", "remainingPercent": 0, "resetsAt": 7_000}}}),
            1_000,
        )
        .unwrap();
        assert_eq!(spend.blocked.unwrap().until, Some(7_000));
    }
}
