use chrono::{DateTime, Local};
use serde::Deserialize;
use serde_json::Value;

const WEEK_MINUTES: u64 = 7 * 24 * 60;

#[derive(Clone, Debug)]
pub struct Window {
    pub minutes: u64,
    pub remaining: Option<f64>,
    pub resets_at: Option<i64>,
}

impl Window {
    pub fn label(&self) -> String {
        if self.minutes == WEEK_MINUTES {
            "週次".into()
        } else if self.minutes.is_multiple_of(60) {
            format!("{}時間", self.minutes / 60)
        } else {
            format!("{}分", self.minutes)
        }
    }

    pub fn remaining_label(&self) -> String {
        match self.remaining {
            Some(value) if value > 0.0 && value < 1.0 => "1%未満".into(),
            Some(value) => format!("{}%", value.floor() as u32),
            None => "—".into(),
        }
    }

    pub fn expired(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|reset| reset <= now)
    }
}

#[derive(Clone, Debug)]
pub struct Usage {
    pub weekly: Option<Window>,
    pub short: Option<Window>,
    pub reset_credits: Option<ResetCredits>,
    pub fetched_at: DateTime<Local>,
}

#[derive(Clone, Debug)]
pub struct ResetCredits {
    pub available_count: u64,
    pub credits: Option<Vec<ResetCredit>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetCredit {
    pub expires_at: Option<i64>,
    status: String,
    reset_type: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawResetCredits {
    available_count: u64,
    credits: Option<Vec<ResetCredit>>,
}

impl ResetCredits {
    fn from_response(value: &Value) -> Option<Self> {
        let raw: RawResetCredits = serde_json::from_value(value.clone()).ok()?;
        let credits = raw.credits.map(|mut credits| {
            credits.retain(|credit| {
                credit.status == "available" && credit.reset_type == "codexRateLimits"
            });
            credits.sort_by_key(|credit| credit.expires_at.unwrap_or(i64::MAX));
            credits
        });
        Some(Self {
            available_count: raw.available_count,
            credits,
        })
    }

    pub fn details_complete(&self) -> bool {
        self.credits
            .as_ref()
            .is_some_and(|credits| credits.len() as u64 == self.available_count)
    }
}

impl Usage {
    pub fn from_response(value: Value) -> Result<Self, &'static str> {
        let bucket =
            if let Some(buckets) = value.get("rateLimitsByLimitId").filter(|v| !v.is_null()) {
                buckets
                    .get("codex")
                    .ok_or("Codexの利用枠を確認できませんでした")?
            } else {
                value
                    .get("rateLimits")
                    .ok_or("利用枠の応答を確認できませんでした")?
            };
        let bucket: RawBucket = serde_json::from_value(bucket.clone())
            .map_err(|_| "利用枠の形式を確認できませんでした")?;
        if bucket.limit_id.as_deref().is_some_and(|id| id != "codex") {
            return Err("Codexの利用枠を確認できませんでした");
        }
        let mut windows: Vec<Window> = [bucket.primary, bucket.secondary]
            .into_iter()
            .flatten()
            .filter_map(RawWindow::normalize)
            .collect();
        windows.sort_by_key(|window| window.minutes);
        Ok(Self {
            weekly: windows.iter().find(|w| w.minutes == WEEK_MINUTES).cloned(),
            short: windows.into_iter().find(|w| w.minutes < WEEK_MINUTES),
            reset_credits: value
                .get("rateLimitResetCredits")
                .and_then(ResetCredits::from_response),
            fetched_at: Local::now(),
        })
    }

    pub fn next_reset(&self, now: i64) -> Option<i64> {
        [&self.weekly, &self.short]
            .into_iter()
            .flatten()
            .filter_map(|window| window.resets_at)
            .chain(
                self.reset_credits
                    .iter()
                    .filter_map(|resets| resets.credits.as_ref())
                    .flatten()
                    .filter_map(|credit| credit.expires_at),
            )
            .filter(|reset| *reset > now)
            .min()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBucket {
    limit_id: Option<String>,
    primary: Option<RawWindow>,
    secondary: Option<RawWindow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWindow {
    used_percent: Option<f64>,
    window_duration_mins: Option<u64>,
    resets_at: Option<i64>,
}

impl RawWindow {
    fn normalize(self) -> Option<Window> {
        let minutes = self.window_duration_mins.filter(|value| *value > 0)?;
        Some(Window {
            minutes,
            remaining: self
                .used_percent
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(|used| (100.0 - used).clamp(0.0, 100.0)),
            resets_at: self.resets_at.filter(|value| *value > 0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reset_credits_keep_the_server_count_and_sort_available_ticket_expiries() {
        let resets = ResetCredits::from_response(&json!({
            "availableCount": 5,
            "credits": [
                {"status": "available", "resetType": "codexRateLimits", "expiresAt": null},
                {"status": "redeemed", "resetType": "codexRateLimits", "expiresAt": 100},
                {"status": "available", "resetType": "codexRateLimits", "expiresAt": 300},
                {"status": "available", "resetType": "unknown", "expiresAt": 50},
                {"status": "available", "resetType": "codexRateLimits", "expiresAt": 200}
            ]
        }))
        .unwrap();
        assert_eq!(resets.available_count, 5);
        assert!(!resets.details_complete());
        assert_eq!(
            resets
                .credits
                .unwrap()
                .iter()
                .map(|credit| credit.expires_at)
                .collect::<Vec<_>>(),
            vec![Some(200), Some(300), None]
        );
        let count_only =
            ResetCredits::from_response(&json!({"availableCount": 2, "credits": null})).unwrap();
        assert_eq!(count_only.available_count, 2);
        assert!(count_only.credits.is_none());
        assert!(!count_only.details_complete());
        assert!(
            ResetCredits::from_response(&json!({"availableCount": 0, "credits": []}))
                .unwrap()
                .details_complete()
        );
        assert!(ResetCredits::from_response(&Value::Null).is_none());
    }

    #[test]
    fn identifies_week_by_duration_and_prefers_codex_bucket() {
        let usage = Usage::from_response(json!({
            "rateLimits": {"primary": {"windowDurationMins": 10080, "usedPercent": 99}},
            "rateLimitsByLimitId": {"codex": {
                "primary": {"windowDurationMins": 10080, "usedPercent": 81},
                "secondary": {"windowDurationMins": 300, "usedPercent": 12}
            }}
        }))
        .unwrap();
        assert_eq!(usage.weekly.unwrap().remaining_label(), "19%");
        assert_eq!(usage.short.unwrap().label(), "5時間");
        assert!(Usage::from_response(json!({"rateLimitsByLimitId": {"other": {}}})).is_err());
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
            let usage = Usage::from_response(json!({"rateLimits": {
                "secondary": {"windowDurationMins": 10080, "usedPercent": used}
            }}))
            .unwrap();
            let week = usage.weekly.unwrap();
            assert_eq!(week.remaining_label(), expected);
            assert_eq!(week.resets_at, None);
        }
        let usage = Usage::from_response(json!({"rateLimits": {"primary": null}})).unwrap();
        assert!(usage.weekly.is_none());
        assert!(usage.short.is_none());
    }
}
