use chrono::{DateTime, Local};
use serde::Deserialize;
use serde_json::Value;

const HOUR: u64 = 60;
const DAY: u64 = 24 * HOUR;
pub const FIVE_HOURS: u64 = 5 * HOUR;
pub const WEEK: u64 = 7 * DAY;
const MONTH: u64 = 30 * DAY;
const YEAR: u64 = 365 * DAY;

/// The length of a usage window as reported by a service.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Span {
    Minutes(u64),
    Unknown,
}

impl Span {
    // Reported lengths can drift slightly; Codex treats lengths within 5% as the same window.
    fn near(self, nominal: u64) -> bool {
        matches!(self, Self::Minutes(minutes)
            if minutes as f64 >= nominal as f64 * 0.95 && minutes as f64 <= nominal as f64 * 1.05)
    }

    pub fn is_weekly(self) -> bool {
        self.near(WEEK)
    }

    pub fn minutes(self) -> Option<u64> {
        match self {
            Self::Minutes(minutes) => Some(minutes),
            Self::Unknown => None,
        }
    }

    pub fn label(self) -> String {
        let Self::Minutes(minutes) = self else {
            return "利用枠".into();
        };
        for (nominal, label) in [
            (FIVE_HOURS, "5時間"),
            (DAY, "日次"),
            (WEEK, "週次"),
            (MONTH, "月次"),
            (YEAR, "年次"),
        ] {
            if self.near(nominal) {
                return label.into();
            }
        }
        if minutes.is_multiple_of(DAY) {
            format!("{}日", minutes / DAY)
        } else if minutes.is_multiple_of(HOUR) {
            format!("{}時間", minutes / HOUR)
        } else {
            format!("{minutes}分")
        }
    }
}

/// A remaining percentage, rounded down so the widget never promises more than is left.
pub fn percent_label(remaining: Option<f64>) -> String {
    match remaining {
        Some(value) if value > 0.0 && value < 1.0 => "1%未満".into(),
        Some(value) => format!("{}%", value.floor() as u32),
        None => "—".into(),
    }
}

#[derive(Clone, Debug)]
pub struct Window {
    pub span: Span,
    pub remaining: Option<f64>,
    pub resets_at: Option<i64>,
}

impl Window {
    pub fn label(&self) -> String {
        self.span.label()
    }

    pub fn remaining_label(&self) -> String {
        percent_label(self.remaining)
    }

    pub fn expired(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|reset| reset <= now)
    }

    /// Used up and not yet reset.
    pub fn exhausted(&self, now: i64) -> bool {
        self.remaining.is_some_and(|value| value <= 0.0) && !self.expired(now)
    }
}

/// Windows that limit the same usage, such as one model family.
#[derive(Clone, Debug, Default)]
pub struct Group {
    /// Distinguishes groups when a service reports more than one.
    pub name: Option<String>,
    /// Ordered from the shortest window to the longest.
    pub windows: Vec<Window>,
}

impl Group {
    pub fn new(name: Option<String>, mut windows: Vec<Window>) -> Self {
        windows.sort_by_key(|window| window.span.minutes().unwrap_or(u64::MAX));
        Self { name, windows }
    }

    /// The window label, prefixed with the group name when there is one.
    pub fn window_label(&self, window: &Window) -> String {
        match &self.name {
            Some(name) => format!("{name}・{}", window.label()),
            None => window.label(),
        }
    }

    fn exhaustion(&self, now: i64) -> Option<Blocked> {
        // Every exhausted window has to reset before the group can be used again.
        let window = self
            .windows
            .iter()
            .filter(|window| window.exhausted(now))
            .max_by_key(|window| window.resets_at.unwrap_or(i64::MAX))?;
        Some(Blocked {
            label: format!("{}の枠", self.window_label(window)),
            until: window.resets_at,
            minutes: window.span.minutes(),
        })
    }
}

/// Usage stops only when every group is used up; it resumes when the first group recovers.
pub fn blocked(groups: &[Group], now: i64) -> Option<Blocked> {
    groups
        .iter()
        .map(|group| group.exhaustion(now))
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .min_by_key(|blocked| blocked.until.unwrap_or(i64::MAX))
}

/// A spending limit, such as a monthly credit allowance.
#[derive(Clone, Debug)]
pub struct Cap {
    pub label: String,
    pub remaining: Option<f64>,
    /// Amounts already formatted with their unit.
    pub detail: Option<String>,
    pub resets_at: Option<i64>,
}

/// The service has stopped ordinary usage until a limit recovers.
#[derive(Clone, Debug, PartialEq)]
pub struct Blocked {
    /// What ran out, such as "5時間の枠".
    pub label: String,
    pub until: Option<i64>,
    /// Length of the exhausted window, for showing progress toward recovery.
    pub minutes: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    /// The first group holds the service's main limits and may be empty.
    pub groups: Vec<Group>,
    pub cap: Option<Cap>,
    /// Remaining prepaid credits, already formatted.
    pub balance: Option<String>,
    pub reset_credits: Option<ResetCredits>,
    pub blocked: Option<Blocked>,
    pub observed_at: DateTime<Local>,
}

impl Snapshot {
    /// The window shown in large type: the main weekly window, otherwise the longest one.
    pub fn main_window(&self) -> Option<&Window> {
        let windows = &self.groups.first()?.windows;
        windows
            .iter()
            .find(|window| window.span.is_weekly())
            .or_else(|| windows.last())
    }

    pub fn next_reset(&self, now: i64) -> Option<i64> {
        self.groups
            .iter()
            .flat_map(|group| &group.windows)
            .filter_map(|window| window.resets_at)
            .chain(self.cap.iter().filter_map(|cap| cap.resets_at))
            .chain(self.blocked.iter().filter_map(|blocked| blocked.until))
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
    pub fn from_response(value: &Value) -> Option<Self> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn window(minutes: u64, remaining: Option<f64>, resets_at: Option<i64>) -> Window {
        Window {
            span: Span::Minutes(minutes),
            remaining,
            resets_at,
        }
    }

    #[test]
    fn spans_share_labels_with_nearby_lengths() {
        for (minutes, label) in [
            (300, "5時間"),
            (290, "5時間"),
            (1440, "日次"),
            (10080, "週次"),
            (10079, "週次"),
            (43200, "月次"),
            (180, "3時間"),
            (2880, "2日"),
            (45, "45分"),
        ] {
            assert_eq!(Span::Minutes(minutes).label(), label, "{minutes}");
        }
        assert!(Span::Minutes(10500).is_weekly());
        assert!(!Span::Minutes(43200).is_weekly());
        assert_eq!(Span::Unknown.label(), "利用枠");
    }

    #[test]
    fn rounding_does_not_invent_remaining_quota() {
        for (remaining, expected) in [
            (Some(100.0), "100%"),
            (Some(0.0), "0%"),
            (Some(0.3), "1%未満"),
            (Some(99.7), "99%"),
            (None, "—"),
        ] {
            assert_eq!(window(10080, remaining, None).remaining_label(), expected);
        }
    }

    #[test]
    fn main_window_prefers_the_weekly_window_of_the_first_group() {
        let snapshot = |groups| Snapshot {
            groups,
            cap: None,
            balance: None,
            reset_credits: None,
            blocked: None,
            observed_at: Local::now(),
        };
        let both = snapshot(vec![Group::new(
            None,
            vec![
                window(10080, Some(62.0), None),
                window(300, Some(88.0), None),
            ],
        )]);
        assert_eq!(both.groups[0].windows[0].label(), "5時間");
        assert_eq!(both.main_window().unwrap().label(), "週次");
        let monthly = snapshot(vec![Group::new(
            None,
            vec![window(300, Some(1.0), None), window(43200, Some(2.0), None)],
        )]);
        assert_eq!(monthly.main_window().unwrap().label(), "月次");
        let extra_only = snapshot(vec![
            Group::default(),
            Group::new(Some("Spark".into()), vec![window(10080, None, None)]),
        ]);
        assert!(extra_only.main_window().is_none());
    }

    #[test]
    fn blocking_waits_for_every_exhausted_window_and_any_recovering_group() {
        let now = 1_000;
        let group = Group::new(
            None,
            vec![
                window(300, Some(0.0), Some(2_000)),
                window(10080, Some(0.0), Some(9_000)),
            ],
        );
        assert_eq!(
            blocked(std::slice::from_ref(&group), now),
            Some(Blocked {
                label: "週次の枠".into(),
                until: Some(9_000),
                minutes: Some(10080),
            })
        );
        // A window whose reset time has passed no longer blocks usage.
        assert!(blocked(std::slice::from_ref(&group), 9_000).is_none());
        let available = Group::new(
            Some("Claude・GPT".into()),
            vec![window(10080, Some(40.0), None)],
        );
        assert!(blocked(&[group.clone(), available], now).is_none());
        let other = Group::new(
            Some("Claude・GPT".into()),
            vec![window(10080, Some(0.0), Some(5_000))],
        );
        assert_eq!(
            blocked(&[group, other], now).unwrap().label,
            "Claude・GPT・週次の枠"
        );
        let unknown = Group::new(None, vec![window(300, Some(0.0), None)]);
        assert_eq!(blocked(&[unknown], now).unwrap().until, None);
        assert!(blocked(&[], now).is_none());
    }

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
}
