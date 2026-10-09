//! Antigravity CLI reports per-bucket quotas to its status line command as `quota`.
use crate::quota::{self, FIVE_HOURS, Group, Snapshot, Span, WEEK, Window};
use chrono::{DateTime, Local};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

const KEPT_FIELDS: [&str; 3] = ["remaining_fraction", "reset_time", "reset_in_seconds"];

pub fn config_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".gemini").join("antigravity-cli"))
}

pub fn settings_path() -> Option<PathBuf> {
    config_dir().map(|directory| directory.join("settings.json"))
}

/// Keep the quota buckets and plan tier; the payload also carries the account email and workspace.
pub fn extract(payload: &Value) -> Option<Value> {
    let buckets = payload.get("quota")?.as_object()?;
    let quota: Map<String, Value> = buckets
        .iter()
        .filter_map(|(key, bucket)| {
            let bucket: Map<String, Value> = bucket
                .as_object()?
                .iter()
                .filter(|(field, _)| KEPT_FIELDS.contains(&field.as_str()))
                .map(|(field, value)| (field.clone(), value.clone()))
                .collect();
            (!bucket.is_empty()).then(|| (key.clone(), Value::Object(bucket)))
        })
        .collect();
    let mut kept = json!({"quota": quota});
    if let Some(tier) = payload.get("plan_tier").and_then(Value::as_str) {
        kept["plan_tier"] = tier.into();
    }
    Some(kept)
}

pub fn snapshot(data: &Value, received_at: DateTime<Local>, now: i64) -> Snapshot {
    let mut groups: Vec<(String, Vec<Window>)> = Vec::new();
    for (key, bucket) in data
        .get("quota")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let (name, span) = bucket_name(key);
        let window = Window {
            span,
            remaining: bucket
                .get("remaining_fraction")
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite())
                .map(|fraction| (fraction * 100.0).clamp(0.0, 100.0)),
            resets_at: reset_time(bucket, received_at.timestamp()),
        };
        match groups.iter_mut().find(|(group, _)| *group == name) {
            Some((_, windows)) => windows.push(window),
            None => groups.push((name, vec![window])),
        }
    }
    // Gemini is the main allowance; other model families follow.
    groups.sort_by_key(|(name, _)| name != "Gemini");
    let groups: Vec<_> = groups
        .into_iter()
        .map(|(name, windows)| Group::new(Some(name), windows))
        .collect();
    let blocked = quota::blocked(&groups, now);
    Snapshot {
        groups,
        cap: None,
        balance: None,
        reset_credits: None,
        blocked,
        observed_at: received_at,
    }
}

/// Bucket ids such as `gemini-5h` and `3p-weekly` name a model family and a window length.
fn bucket_name(key: &str) -> (String, Span) {
    let span = |suffix| match suffix {
        "5h" => Some(Span::Minutes(FIVE_HOURS)),
        "weekly" => Some(Span::Minutes(WEEK)),
        "daily" => Some(Span::Minutes(24 * 60)),
        _ => None,
    };
    if let Some((family, suffix)) = key.rsplit_once('-')
        && let Some(span) = span(suffix)
    {
        let name = match family {
            "gemini" => "Gemini".into(),
            // Third-party models share one allowance.
            "3p" => "Claude・GPT".into(),
            other => other.into(),
        };
        return (name, span);
    }
    (key.into(), Span::Unknown)
}

fn reset_time(bucket: &Value, received_at: i64) -> Option<i64> {
    bucket
        .get("reset_time")
        .and_then(Value::as_str)
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.timestamp())
        .or_else(|| {
            let seconds = bucket.get("reset_in_seconds")?.as_i64()?;
            Some(received_at + seconds)
        })
        .filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const NOW: i64 = 1_783_000_000;

    #[test]
    fn only_quota_buckets_and_plan_tier_are_kept() {
        let payload = json!({
            "email": "someone@example.com",
            "workspace": {"path": "C:/Users/someone/project"},
            "plan_tier": "pro",
            "quota": {
                "gemini-weekly": {"remaining_fraction": 0.9378, "reset_time": "2026-07-06T07:50:32Z",
                    "reset_in_seconds": 560580, "label": "x"},
                "empty": {"other": 1}
            }
        });
        assert_eq!(
            extract(&payload).unwrap(),
            json!({
                "quota": {"gemini-weekly": {"remaining_fraction": 0.9378,
                    "reset_time": "2026-07-06T07:50:32Z", "reset_in_seconds": 560580}},
                "plan_tier": "pro"
            })
        );
        assert!(extract(&json!({"email": "someone@example.com"})).is_none());
    }

    #[test]
    fn buckets_become_model_families_with_their_windows() {
        let received = Local.timestamp_opt(NOW, 0).unwrap();
        let usage = snapshot(
            &json!({"quota": {
                "3p-weekly": {"remaining_fraction": 1.0, "reset_in_seconds": 3600},
                "gemini-5h": {"remaining_fraction": 0.95, "reset_time": "2026-07-06T07:50:32Z"},
                "gemini-weekly": {"remaining_fraction": 0.84},
                "gemini-3-pro-high": {"remaining_fraction": 0.5}
            }}),
            received,
            NOW,
        );
        let names: Vec<_> = usage
            .groups
            .iter()
            .map(|group| group.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["Gemini", "Claude・GPT", "gemini-3-pro-high"]);
        let gemini = &usage.groups[0];
        let labels: Vec<_> = gemini
            .windows
            .iter()
            .map(|window| gemini.window_label(window))
            .collect();
        assert_eq!(labels, ["Gemini・5時間", "Gemini・週次"]);
        assert_eq!(usage.main_window().unwrap().remaining_label(), "84%");
        assert_eq!(usage.groups[1].windows[0].resets_at, Some(NOW + 3600));
        assert_eq!(usage.groups[2].windows[0].label(), "利用枠");
        assert!(usage.blocked.is_none());
    }

    #[test]
    fn usage_is_blocked_only_when_every_family_is_used_up() {
        let received = Local.timestamp_opt(NOW, 0).unwrap();
        let partial = snapshot(
            &json!({"quota": {
                "gemini-5h": {"remaining_fraction": 0.0, "reset_in_seconds": 600},
                "3p-weekly": {"remaining_fraction": 0.4, "reset_in_seconds": 86400}
            }}),
            received,
            NOW,
        );
        assert!(partial.blocked.is_none());
        let all = snapshot(
            &json!({"quota": {
                "gemini-5h": {"remaining_fraction": 0.0, "reset_in_seconds": 600},
                "3p-weekly": {"remaining_fraction": 0.0, "reset_in_seconds": 86400}
            }}),
            received,
            NOW,
        );
        let blocked = all.blocked.unwrap();
        assert_eq!(blocked.label, "Gemini・5時間の枠");
        assert_eq!(blocked.until, Some(NOW + 600));
    }
}
