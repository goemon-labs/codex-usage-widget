//! Claude Code reports subscription usage to its status line command as `rate_limits`.
use crate::quota::{self, Blocked, Cap, FIVE_HOURS, Group, Snapshot, Span, WEEK, Window};
use chrono::{DateTime, Local};
use serde_json::{Map, Value};
use std::{env, path::PathBuf};

/// Fields worth keeping from each `rate_limits` entry; everything else describes the session.
const KEPT_FIELDS: [&str; 7] = [
    "used_percentage",
    "resets_at",
    "used_usd",
    "limit_usd",
    "period",
    "model",
    "display_name",
];

pub fn config_dir() -> Option<PathBuf> {
    env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".claude")))
}

pub fn settings_path() -> Option<PathBuf> {
    config_dir().map(|directory| directory.join("settings.json"))
}

/// Keep only the usage numbers; absent until a subscriber's session receives its first response.
pub fn extract(payload: &Value) -> Option<Value> {
    let limits = payload.get("rate_limits")?.as_object()?;
    let kept: Map<String, Value> = limits
        .iter()
        .filter_map(|(key, entry)| {
            let entry: Map<String, Value> = entry
                .as_object()?
                .iter()
                .filter(|(field, _)| KEPT_FIELDS.contains(&field.as_str()))
                .map(|(field, value)| (field.clone(), value.clone()))
                .collect();
            (!entry.is_empty()).then(|| (key.clone(), Value::Object(entry)))
        })
        .collect();
    Some(Value::Object(kept))
}

pub fn snapshot(data: &Value, received_at: DateTime<Local>, now: i64) -> Snapshot {
    let mut main = Vec::new();
    let mut others = Vec::new();
    let mut cap = None;
    for (key, entry) in data.as_object().into_iter().flatten() {
        match key.as_str() {
            "five_hour" => main.extend(window(entry, FIVE_HOURS)),
            "seven_day" => main.extend(window(entry, WEEK)),
            "spend_limit" => cap = spend_limit(entry),
            // Windows added later, such as a model's own weekly limit, get their own group.
            _ => {
                let minutes = if key.starts_with("five_hour") {
                    FIVE_HOURS
                } else if key.starts_with("seven_day") {
                    WEEK
                } else {
                    continue;
                };
                let name = ["model", "display_name"]
                    .into_iter()
                    .find_map(|field| entry.get(field).and_then(Value::as_str))
                    .map_or_else(|| key.clone(), String::from);
                others.extend(
                    window(entry, minutes).map(|window| Group::new(Some(name), vec![window])),
                );
            }
        }
    }
    let mut groups = vec![Group::new(None, main)];
    groups.extend(others);
    let blocked = quota::blocked(&groups[..1], now).or_else(|| {
        let cap = cap.as_ref()?;
        cap.remaining
            .is_some_and(|remaining| remaining <= 0.0)
            .then(|| Blocked {
                label: cap.label.clone(),
                until: cap.resets_at,
                minutes: None,
            })
    });
    Snapshot {
        groups,
        cap,
        balance: None,
        reset_credits: None,
        blocked,
        observed_at: received_at,
    }
}

fn window(entry: &Value, minutes: u64) -> Option<Window> {
    let used = used_percentage(entry)?;
    Some(Window {
        span: Span::Minutes(minutes),
        remaining: Some((100.0 - used).clamp(0.0, 100.0)),
        resets_at: timestamp(entry.get("resets_at")),
    })
}

fn spend_limit(entry: &Value) -> Option<Cap> {
    let used = used_percentage(entry)?;
    let period = match entry.get("period").and_then(Value::as_str) {
        Some("daily") => "今日の",
        Some("weekly") => "今週の",
        Some("monthly") => "今月の",
        _ => "",
    };
    let amount = |field| entry.get(field).and_then(Value::as_f64);
    Some(Cap {
        label: format!("{period}利用額の上限"),
        remaining: Some((100.0 - used).clamp(0.0, 100.0)),
        detail: amount("used_usd")
            .zip(amount("limit_usd"))
            .map(|(used, limit)| {
                format!(
                    "${} / ${} 使用",
                    quota::amount_label(used),
                    quota::amount_label(limit)
                )
            }),
        resets_at: timestamp(entry.get("resets_at")),
    })
}

fn used_percentage(entry: &Value) -> Option<f64> {
    entry
        .get("used_percentage")?
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Reset times are documented as Unix seconds; some versions have sent ISO 8601 text.
fn timestamp(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|time| time.timestamp()),
        _ => None,
    }
    .filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_738_000_000;

    #[test]
    fn only_usage_numbers_are_kept_from_the_session_payload() {
        let payload = json!({
            "session_id": "private",
            "cwd": "C:/Users/someone/project",
            "workspace": {"current_dir": "C:/Users/someone/project"},
            "rate_limits": {
                "five_hour": {"used_percentage": 23.5, "resets_at": 1738425600, "note": "x"},
                "seven_day": {"used_percentage": 41.2, "resets_at": 1738857600},
                "empty": {"unknown": 1},
                "broken": 3
            }
        });
        assert_eq!(
            extract(&payload).unwrap(),
            json!({
                "five_hour": {"used_percentage": 23.5, "resets_at": 1738425600},
                "seven_day": {"used_percentage": 41.2, "resets_at": 1738857600}
            })
        );
        assert!(extract(&json!({"model": {"display_name": "Opus"}})).is_none());
    }

    #[test]
    fn session_and_weekly_windows_become_the_main_group() {
        let usage = snapshot(
            &json!({
                "five_hour": {"used_percentage": 23.5, "resets_at": NOW + 3600},
                "seven_day": {"used_percentage": 41.2, "resets_at": "2025-02-06T16:00:00Z"},
                "seven_day_model": {"model": "Fable", "used_percentage": 42, "resets_at": NOW + 7200}
            }),
            Local::now(),
            NOW,
        );
        let main = &usage.groups[0];
        let labels: Vec<_> = main
            .windows
            .iter()
            .map(|window| (window.label(), window.remaining_label()))
            .collect();
        assert_eq!(
            labels,
            [
                ("5時間".into(), "76%".into()),
                ("週次".into(), "58%".into())
            ]
        );
        assert_eq!(main.windows[1].resets_at, Some(1_738_857_600));
        assert_eq!(usage.main_window().unwrap().label(), "週次");
        let fable = &usage.groups[1];
        assert_eq!(fable.window_label(&fable.windows[0]), "Fable・週次");
        assert!(usage.blocked.is_none());
    }

    #[test]
    fn an_exhausted_window_or_spend_limit_blocks_usage() {
        let usage = snapshot(
            &json!({"five_hour": {"used_percentage": 100, "resets_at": NOW + 600},
                    "seven_day": {"used_percentage": 30, "resets_at": NOW + 86400}}),
            Local::now(),
            NOW,
        );
        assert_eq!(
            usage.blocked,
            Some(Blocked {
                label: "5時間の枠".into(),
                until: Some(NOW + 600),
                minutes: Some(FIVE_HOURS),
            })
        );
        let spend = snapshot(
            &json!({"spend_limit": {"used_percentage": 104, "resets_at": NOW + 900,
                    "used_usd": 520.5, "limit_usd": 500, "period": "monthly"}}),
            Local::now(),
            NOW,
        );
        let cap = spend.cap.as_ref().unwrap();
        assert_eq!(cap.label, "今月の利用額の上限");
        assert_eq!(cap.detail.as_deref(), Some("$521 / $500 使用"));
        assert!(spend.main_window().is_none());
        assert_eq!(spend.blocked.unwrap().until, Some(NOW + 900));
        let percent_only = snapshot(
            &json!({"spend_limit": {"used_percentage": 62.8, "resets_at": NOW + 900}}),
            Local::now(),
            NOW,
        );
        assert!(percent_only.cap.unwrap().detail.is_none());
        assert!(percent_only.blocked.is_none());
    }
}
