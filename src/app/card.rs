//! What one service's usage looks like, shared by the single card and the combined view.
use super::{AMBER, BORDER, FOREGROUND, GREEN, MUTED, reset_label};
use crate::quota::{self, Blocked, Cap, Group, Snapshot, Window};
use eframe::egui::{self, Align, Layout, RichText, Sense, vec2};

/// How a service's numbers arrive, which changes how missing or stale data is described.
pub(super) struct Source {
    pub name: &'static str,
    /// Delivered by the service's own tool while it is in use, rather than fetched by the widget.
    pub received: bool,
    /// A fetch is running and nothing has arrived yet.
    pub loading: bool,
}

/// The large figure at the top of a card.
#[derive(Debug, PartialEq)]
pub(super) struct Hero {
    pub label: String,
    pub value: String,
    pub fraction: Option<f32>,
    pub blocked: bool,
    pub detail: Option<String>,
    pub caption: &'static str,
    pub when: String,
}

/// A limit listed below the large figure.
#[derive(Debug, PartialEq)]
pub(super) struct Line {
    pub title: String,
    pub value: String,
    pub fraction: Option<f32>,
    pub note: Option<String>,
    pub alert: bool,
}

pub(super) fn hero(snapshot: Option<&Snapshot>, source: &Source, now: i64) -> Hero {
    if let Some(snapshot) = snapshot {
        if let Some(blocked) = &snapshot.blocked {
            return blocked_hero(blocked, now);
        }
        if let Some(window) = snapshot.main_window() {
            return window_hero(&snapshot.groups[0], window, source, now);
        }
        if let Some(cap) = &snapshot.cap {
            return cap_hero(cap);
        }
    }
    Hero {
        label: "週次の残り".into(),
        value: "—".into(),
        fraction: None,
        blocked: false,
        detail: None,
        caption: "リセット",
        when: if snapshot.is_some() {
            "利用枠の情報を取得できませんでした".into()
        } else if source.loading {
            "取得中…".into()
        } else if source.received {
            "まだ受信していません".into()
        } else {
            format!("{}の利用枠を確認します", source.name)
        },
    }
}

fn window_hero(group: &Group, window: &Window, source: &Source, now: i64) -> Hero {
    let expired = window.expired(now);
    Hero {
        label: format!("{}の残り", group.window_label(window)),
        value: if expired {
            "—".into()
        } else {
            window.remaining_label()
        },
        fraction: fraction(window.remaining).filter(|_| !expired),
        blocked: false,
        detail: None,
        caption: "リセット",
        when: if expired && source.received {
            "リセット済み（次回の利用で更新）".into()
        } else if expired {
            "リセット後の情報を確認中".into()
        } else {
            window
                .resets_at
                .map_or("リセット日時を確認できませんでした".into(), reset_label)
        },
    }
}

fn cap_hero(cap: &Cap) -> Hero {
    Hero {
        label: format!("{}の残り", cap.label),
        value: quota::percent_label(cap.remaining),
        fraction: fraction(cap.remaining),
        blocked: false,
        detail: cap.detail.clone(),
        caption: "リセット",
        when: cap
            .resets_at
            .map_or("リセット日時を確認できませんでした".into(), reset_label),
    }
}

fn blocked_hero(blocked: &Blocked, now: i64) -> Hero {
    Hero {
        label: format!("リキャスト中（{}）", blocked.label),
        value: match blocked.until {
            Some(until) if until > now => countdown_label(until, now),
            Some(_) => "回復を確認中".into(),
            None => "上限に達しました".into(),
        },
        // The bar fills up as the exhausted window approaches its reset.
        fraction: blocked.until.zip(blocked.minutes).map(|(until, minutes)| {
            let length = (minutes * 60).max(1) as f32;
            (1.0 - (until - now).max(0) as f32 / length).clamp(0.0, 1.0)
        }),
        blocked: true,
        detail: None,
        caption: "回復予定",
        when: blocked
            .until
            .map_or("回復時刻を確認できませんでした".into(), reset_label),
    }
}

/// Every limit not already shown as the large figure.
pub(super) fn lines(snapshot: &Snapshot, received: bool, now: i64) -> Vec<Line> {
    let main = snapshot
        .groups
        .first()
        .map_or(&[][..], |group| group.windows.as_slice());
    let hero_window = match &snapshot.blocked {
        // The large figure already counts down to the window that stopped usage.
        Some(blocked) => main
            .iter()
            .find(|window| window.exhausted(now) && window.resets_at == blocked.until),
        None => snapshot.main_window(),
    };
    let mut lines: Vec<Line> = snapshot
        .groups
        .iter()
        .flat_map(|group| group.windows.iter().map(move |window| (group, window)))
        .filter(|(_, window)| !hero_window.is_some_and(|hero| std::ptr::eq(hero, *window)))
        .map(|(group, window)| window_line(group, window, received, now))
        .collect();
    let cap_is_hero = snapshot.blocked.is_none() && snapshot.main_window().is_none();
    if let Some(cap) = snapshot.cap.as_ref().filter(|_| !cap_is_hero) {
        lines.push(Line {
            title: format!("{}の残り", cap.label),
            value: quota::percent_label(cap.remaining),
            fraction: fraction(cap.remaining),
            note: cap.detail.clone(),
            alert: false,
        });
    }
    if let Some(balance) = &snapshot.balance {
        lines.push(Line {
            title: "クレジット残高".into(),
            value: balance.clone(),
            fraction: None,
            note: None,
            alert: false,
        });
    }
    lines
}

fn window_line(group: &Group, window: &Window, received: bool, now: i64) -> Line {
    let title = format!("{}の残り", group.window_label(window));
    if window.exhausted(now) {
        return Line {
            title,
            value: window
                .resets_at
                .map_or("0%".into(), |reset| countdown_label(reset, now)),
            fraction: None,
            note: window
                .resets_at
                .map(|reset| format!("{} に回復", reset_label(reset))),
            alert: true,
        };
    }
    let expired = window.expired(now);
    Line {
        title,
        value: match (expired, received) {
            (true, true) => "リセット済み".into(),
            (true, false) => "確認中".into(),
            (false, _) => window.remaining_label(),
        },
        fraction: fraction(window.remaining).filter(|_| !expired),
        note: window.resets_at.map(reset_label),
        alert: false,
    }
}

fn fraction(percent: Option<f64>) -> Option<f32> {
    percent.map(|value| (value / 100.0).clamp(0.0, 1.0) as f32)
}

/// Time left until a limit recovers, rounded up so it never reads zero too early.
pub(super) fn countdown_label(until: i64, now: i64) -> String {
    let minutes = ((until - now).max(0) + 59) / 60;
    if minutes >= 24 * 60 {
        format!(
            "あと {}日{}時間",
            minutes / (24 * 60),
            minutes % (24 * 60) / 60
        )
    } else if minutes >= 60 {
        format!("あと {}時間{}分", minutes / 60, minutes % 60)
    } else {
        format!("あと {}分", minutes.max(1))
    }
}

/// One-line text for the bar mode.
pub(super) fn bar_text(hero: &Hero) -> String {
    if hero.blocked {
        format!("リキャスト中 {}", hero.value)
    } else {
        format!("{} {}", hero.label, hero.value)
    }
}

pub(super) fn hero_ui(ui: &mut egui::Ui, hero: &Hero) {
    ui.add_space(17.0);
    ui.label(RichText::new(&hero.label).size(11.0).color(MUTED));
    ui.add_space(1.0);
    // A countdown is longer than a percentage; keep it on one line inside the card.
    let (size, color) = if hero.blocked {
        (26.0, AMBER)
    } else {
        (32.0, FOREGROUND)
    };
    ui.label(RichText::new(&hero.value).size(size).color(color));
    ui.add_space(7.0);
    let fill = if hero.blocked { AMBER } else { GREEN };
    bar_ui(ui, ui.available_width(), hero.fraction, fill);
    if let Some(detail) = &hero.detail {
        ui.add_space(8.0);
        ui.label(RichText::new(detail).size(12.0));
    }
    ui.add_space(12.0);
    ui.label(RichText::new(hero.caption).size(11.0).color(MUTED));
    ui.label(RichText::new(&hero.when).size(12.0));
}

pub(super) fn line_ui(ui: &mut egui::Ui, line: &Line) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(&line.title).size(11.0).color(MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let color = if line.alert { AMBER } else { FOREGROUND };
            ui.label(RichText::new(&line.value).size(12.0).color(color));
        });
    });
    if let Some(note) = &line.note {
        ui.label(RichText::new(note).size(11.0).color(MUTED));
    }
}

pub(super) fn bar_ui(ui: &mut egui::Ui, width: f32, fraction: Option<f32>, fill: egui::Color32) {
    let (bar, _) = ui.allocate_exact_size(vec2(width, 4.0), Sense::hover());
    ui.painter().rect_filled(bar, 2.0, BORDER);
    if let Some(fraction) = fraction.filter(|fraction| *fraction > 0.0) {
        let filled = egui::Rect::from_min_size(bar.min, vec2(bar.width() * fraction, bar.height()));
        ui.painter().rect_filled(filled, 2.0, fill);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::Span;
    use chrono::Local;

    const NOW: i64 = 1_000_000;

    fn window(minutes: u64, remaining: f64, resets_at: Option<i64>) -> Window {
        Window {
            span: Span::Minutes(minutes),
            remaining: Some(remaining),
            resets_at,
        }
    }

    fn snapshot(groups: Vec<Group>, blocked: Option<Blocked>) -> Snapshot {
        Snapshot {
            groups,
            cap: None,
            balance: None,
            reset_credits: None,
            blocked,
            observed_at: Local::now(),
        }
    }

    const CODEX: Source = Source {
        name: "Codex",
        received: false,
        loading: false,
    };
    const CLAUDE: Source = Source {
        name: "Claude Code",
        received: true,
        loading: false,
    };

    #[test]
    fn the_weekly_window_leads_and_the_rest_are_listed() {
        let usage = snapshot(
            vec![
                Group::new(
                    None,
                    vec![
                        window(10080, 62.0, Some(NOW + 3_600)),
                        window(300, 88.0, None),
                    ],
                ),
                Group::new(Some("Claude・GPT".into()), vec![window(10080, 100.0, None)]),
            ],
            None,
        );
        let hero = hero(Some(&usage), &CODEX, NOW);
        assert_eq!(
            (hero.label.as_str(), hero.value.as_str()),
            ("週次の残り", "62%")
        );
        assert_eq!(bar_text(&hero), "週次の残り 62%");
        let titles: Vec<_> = lines(&usage, false, NOW)
            .into_iter()
            .map(|line| (line.title, line.value))
            .collect();
        assert_eq!(
            titles,
            [
                ("5時間の残り".to_string(), "88%".to_string()),
                ("Claude・GPT・週次の残り".into(), "100%".into())
            ]
        );
    }

    #[test]
    fn a_blocked_service_counts_down_and_lists_the_other_windows() {
        let usage = snapshot(
            vec![Group::new(
                None,
                vec![
                    window(300, 0.0, Some(NOW + 2 * 3600 + 13 * 60)),
                    window(10080, 54.0, Some(NOW + 86_400)),
                ],
            )],
            Some(Blocked {
                label: "5時間の枠".into(),
                until: Some(NOW + 2 * 3600 + 13 * 60),
                minutes: Some(300),
            }),
        );
        let hero = hero(Some(&usage), &CLAUDE, NOW);
        assert_eq!(hero.label, "リキャスト中（5時間の枠）");
        assert_eq!(hero.value, "あと 2時間13分");
        assert_eq!(bar_text(&hero), "リキャスト中 あと 2時間13分");
        assert!(
            hero.fraction
                .is_some_and(|fraction| (fraction - 0.556).abs() < 0.01)
        );
        let lines = lines(&usage, true, NOW);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].title, "週次の残り");
    }

    #[test]
    fn credit_plans_lead_with_the_cap_and_list_the_balance() {
        let mut usage = snapshot(vec![Group::default()], None);
        usage.cap = Some(Cap {
            label: "月間クレジット上限".into(),
            remaining: Some(62.4),
            detail: Some("314 / 500 クレジット使用".into()),
            resets_at: None,
        });
        usage.balance = Some("1250".into());
        let hero = hero(Some(&usage), &CODEX, NOW);
        assert_eq!(hero.label, "月間クレジット上限の残り");
        assert_eq!(hero.value, "62%");
        assert_eq!(hero.detail.as_deref(), Some("314 / 500 クレジット使用"));
        let lines = lines(&usage, false, NOW);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            (lines[0].title.as_str(), lines[0].value.as_str()),
            ("クレジット残高", "1250")
        );
    }

    #[test]
    fn stale_and_missing_data_are_described_by_how_the_service_reports() {
        let expired = snapshot(
            vec![Group::new(
                None,
                vec![
                    window(10080, 40.0, Some(NOW - 1)),
                    window(300, 10.0, Some(NOW - 1)),
                ],
            )],
            None,
        );
        assert_eq!(
            hero(Some(&expired), &CODEX, NOW).when,
            "リセット後の情報を確認中"
        );
        assert_eq!(
            hero(Some(&expired), &CLAUDE, NOW).when,
            "リセット済み（次回の利用で更新）"
        );
        assert_eq!(lines(&expired, false, NOW)[0].value, "確認中");
        assert_eq!(lines(&expired, true, NOW)[0].value, "リセット済み");
        assert_eq!(hero(None, &CLAUDE, NOW).when, "まだ受信していません");
        assert_eq!(hero(None, &CODEX, NOW).when, "Codexの利用枠を確認します");
        let loading = Source {
            loading: true,
            ..CODEX
        };
        assert_eq!(hero(None, &loading, NOW).when, "取得中…");
    }

    #[test]
    fn countdowns_round_up_to_the_next_minute() {
        assert_eq!(countdown_label(NOW + 1, NOW), "あと 1分");
        assert_eq!(countdown_label(NOW + 59 * 60 + 1, NOW), "あと 1時間0分");
        assert_eq!(
            countdown_label(NOW + 3 * 86_400 + 4 * 3600, NOW),
            "あと 3日4時間"
        );
    }
}
