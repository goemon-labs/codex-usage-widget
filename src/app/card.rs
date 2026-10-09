//! What one service's usage looks like, shared by the single card and the combined view.
use super::{AMBER, BORDER, FOREGROUND, GREEN, MUTED, reset_label};
use crate::quota::{self, Blocked, Cap, Group, Snapshot, Window};
use eframe::egui::{self, Align, Layout, RichText, Sense, vec2};

/// How a service's numbers arrive, which changes how missing or stale data is described.
pub(super) struct Source {
    /// Delivered by the service's own tool while it is in use, rather than fetched by the widget.
    pub received: bool,
}

/// A large figure: what is left of a limit, or how long until a used-up limit resets.
#[derive(Debug, PartialEq)]
pub(super) struct Hero {
    pub label: String,
    pub value: String,
    /// The value in a few characters, for the one-line bar.
    pub short_value: String,
    /// Time until usage resumes, such as "2時間13分", while a limit is used up.
    pub wait: Option<String>,
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

/// A block still worth showing. Received numbers only refresh while the tool runs, so a block
/// whose reset time has passed is left to the stale window display instead.
fn active_block(snapshot: &Snapshot, received: bool, now: i64) -> Option<&Blocked> {
    snapshot
        .blocked
        .as_ref()
        .filter(|blocked| !(received && blocked.until.is_some_and(|until| until <= now)))
}

pub(super) fn hero(snapshot: Option<&Snapshot>, source: &Source, now: i64) -> Hero {
    if let Some(snapshot) = snapshot {
        if let Some(blocked) = active_block(snapshot, source.received, now) {
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
        short_value: "—".into(),
        wait: None,
        fraction: None,
        blocked: false,
        detail: None,
        caption: "リセット",
        when: if snapshot.is_some() {
            "残量を確認できませんでした".into()
        } else if source.received {
            "まだデータがありません".into()
        } else {
            "確認中…".into()
        },
    }
}

fn window_hero(group: &Group, window: &Window, source: &Source, now: i64) -> Hero {
    let expired = window.expired(now);
    let value = if expired {
        "—".into()
    } else {
        window.remaining_label()
    };
    Hero {
        label: format!("{}の残り", group.window_label(window)),
        short_value: value.clone(),
        value,
        wait: None,
        fraction: fraction(window.remaining).filter(|_| !expired),
        blocked: false,
        detail: None,
        caption: "リセット",
        when: if expired && source.received {
            "リセット済み".into()
        } else if expired {
            "確認中…".into()
        } else {
            window.resets_at.map_or("不明".into(), reset_label)
        },
    }
}

fn cap_hero(cap: &Cap) -> Hero {
    let value = quota::percent_label(cap.remaining);
    Hero {
        label: format!("{}の残り", cap.label),
        short_value: value.clone(),
        value,
        wait: None,
        fraction: fraction(cap.remaining),
        blocked: false,
        detail: cap.detail.clone(),
        caption: "リセット",
        when: cap.resets_at.map_or("不明".into(), reset_label),
    }
}

fn blocked_hero(blocked: &Blocked, now: i64) -> Hero {
    let wait = blocked
        .until
        .filter(|until| *until > now)
        .map(|until| duration_label(until, now));
    Hero {
        label: format!("{}の上限に達しました", blocked.label),
        value: match (&wait, blocked.until) {
            (Some(wait), _) => format!("あと {wait}"),
            (None, Some(_)) => "確認中…".into(),
            (None, None) => "—".into(),
        },
        short_value: "上限".into(),
        wait,
        // The bar fills up as the used-up limit approaches its reset.
        fraction: blocked.until.zip(blocked.minutes).map(|(until, minutes)| {
            let length = (minutes * 60).max(1) as f32;
            (1.0 - (until - now).max(0) as f32 / length).clamp(0.0, 1.0)
        }),
        blocked: true,
        detail: None,
        caption: "リセット",
        when: blocked.until.map_or("不明".into(), reset_label),
    }
}

/// Every window as its own large figure, for the combined view and for services whose
/// model families each have their own limits.
pub(super) fn blocks(snapshot: &Snapshot, source: &Source, now: i64) -> Vec<Hero> {
    let mut blocks: Vec<Hero> = snapshot
        .groups
        .iter()
        .flat_map(|group| group.windows.iter().map(move |window| (group, window)))
        .map(|(group, window)| {
            if window.exhausted(now) {
                let blocked = Blocked {
                    label: group.window_label(window),
                    until: window.resets_at,
                    minutes: window.span.minutes(),
                };
                blocked_hero(&blocked, now)
            } else {
                window_hero(group, window, source, now)
            }
        })
        .collect();
    if blocks.is_empty()
        && let Some(cap) = &snapshot.cap
    {
        blocks.push(cap_hero(cap));
    }
    // A stop that no window explains, such as a spending cap, comes first.
    if let Some(blocked) = active_block(snapshot, source.received, now)
        && !blocks.iter().any(|block| block.blocked)
    {
        blocks.insert(0, blocked_hero(blocked, now));
    }
    blocks
}

/// Whether a service reports separate limits for more than one model family.
pub(super) fn has_families(snapshot: &Snapshot) -> bool {
    snapshot
        .groups
        .iter()
        .filter(|group| !group.windows.is_empty())
        .count()
        > 1
}

/// Every limit not already shown as the large figure.
pub(super) fn lines(snapshot: &Snapshot, received: bool, now: i64) -> Vec<Line> {
    let main = snapshot
        .groups
        .first()
        .map_or(&[][..], |group| group.windows.as_slice());
    let block = active_block(snapshot, received, now);
    let hero_window = match block {
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
    let cap_is_hero = block.is_none() && snapshot.main_window().is_none();
    lines.extend(money_lines(snapshot, !cap_is_hero));
    lines
}

/// The spending cap, unless it is already the large figure, and the prepaid balance.
pub(super) fn money_lines(snapshot: &Snapshot, with_cap: bool) -> Vec<Line> {
    let cap = snapshot.cap.as_ref().filter(|_| with_cap).map(|cap| Line {
        title: format!("{}の残り", cap.label),
        value: quota::percent_label(cap.remaining),
        fraction: fraction(cap.remaining),
        note: cap.detail.clone(),
        alert: cap.remaining.is_some_and(|remaining| remaining <= 0.0),
    });
    let balance = snapshot.balance.as_ref().map(|balance| Line {
        title: "クレジット残高".into(),
        value: balance.clone(),
        fraction: None,
        note: None,
        alert: false,
    });
    cap.into_iter().chain(balance).collect()
}

fn window_line(group: &Group, window: &Window, received: bool, now: i64) -> Line {
    let title = format!("{}の残り", group.window_label(window));
    if window.exhausted(now) {
        return Line {
            title,
            value: window.resets_at.map_or("0%".into(), |reset| {
                format!("あと {}", duration_label(reset, now))
            }),
            fraction: None,
            note: window
                .resets_at
                .map(|reset| format!("{} にリセット", reset_label(reset))),
            alert: true,
        };
    }
    let expired = window.expired(now);
    Line {
        title,
        value: match (expired, received) {
            (true, true) => "リセット済み".into(),
            (true, false) => "確認中…".into(),
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

/// Time left until `until`, rounded up so it never reads zero too early.
fn duration_label(until: i64, now: i64) -> String {
    let minutes = ((until - now).max(0) + 59) / 60;
    if minutes >= 24 * 60 {
        format!("{}日{}時間", minutes / (24 * 60), minutes % (24 * 60) / 60)
    } else if minutes >= 60 {
        format!("{}時間{}分", minutes / 60, minutes % 60)
    } else {
        format!("{}分", minutes.max(1))
    }
}

/// One-line text for the bar mode.
pub(super) fn bar_text(hero: &Hero) -> String {
    match (&hero.wait, hero.blocked) {
        (Some(wait), _) => format!("利用再開まで {wait}"),
        (None, true) => hero.label.clone(),
        (None, false) => format!("{} {}", hero.label, hero.value),
    }
}

/// Windows that reset together share one reset line, shown after the last of them.
fn shares_reset(block: &Hero, next: &Hero) -> bool {
    block.caption == next.caption && block.when == next.when
}

/// Large figures one below another, each laid out as on the single card.
pub(super) fn stack_ui(ui: &mut egui::Ui, blocks: &[Hero], first_gap: f32) {
    for (index, block) in blocks.iter().enumerate() {
        ui.add_space(if index == 0 { first_gap } else { 14.0 });
        ui.label(RichText::new(&block.label).size(11.0).color(MUTED));
        ui.add_space(1.0);
        // A countdown is longer than a percentage; keep it on one line inside the card.
        let (size, color) = if block.blocked {
            (26.0, AMBER)
        } else {
            (32.0, FOREGROUND)
        };
        ui.label(RichText::new(&block.value).size(size).color(color));
        ui.add_space(7.0);
        let fill = if block.blocked { AMBER } else { GREEN };
        bar_ui(ui, ui.available_width(), block.fraction, fill);
        if let Some(detail) = &block.detail {
            ui.add_space(8.0);
            ui.label(RichText::new(detail).size(12.0));
        }
        if !blocks
            .get(index + 1)
            .is_some_and(|next| shares_reset(block, next))
        {
            ui.add_space(12.0);
            ui.label(RichText::new(block.caption).size(11.0).color(MUTED));
            ui.label(RichText::new(&block.when).size(12.0));
        }
    }
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

    const CODEX: Source = Source { received: false };
    const CLAUDE: Source = Source { received: true };

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
        assert!(has_families(&usage));
        let labels: Vec<_> = blocks(&usage, &CODEX, NOW)
            .into_iter()
            .map(|block| block.label)
            .collect();
        assert_eq!(
            labels,
            ["5時間の残り", "週次の残り", "Claude・GPT・週次の残り"]
        );
    }

    #[test]
    fn a_used_up_limit_shows_the_time_until_it_resets() {
        let until = NOW + 2 * 3600 + 13 * 60;
        let usage = snapshot(
            vec![Group::new(
                None,
                vec![
                    window(300, 0.0, Some(until)),
                    window(10080, 54.0, Some(NOW + 86_400)),
                ],
            )],
            Some(Blocked {
                label: "5時間".into(),
                until: Some(until),
                minutes: Some(300),
            }),
        );
        let hero = hero(Some(&usage), &CLAUDE, NOW);
        assert_eq!(hero.label, "5時間の上限に達しました");
        assert_eq!(hero.value, "あと 2時間13分");
        assert_eq!(hero.short_value, "上限");
        assert_eq!(hero.caption, "リセット");
        assert_eq!(bar_text(&hero), "利用再開まで 2時間13分");
        assert!(
            hero.fraction
                .is_some_and(|fraction| (fraction - 0.556).abs() < 0.01)
        );
        let lines = lines(&usage, true, NOW);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].title, "週次の残り");
        // Each window keeps its own figure; the used-up one counts down.
        let blocks = blocks(&usage, &CLAUDE, NOW);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], hero);
        assert_eq!(blocks[1].label, "週次の残り");
        // Received numbers past their reset no longer claim usage is stopped.
        let later = super::hero(Some(&usage), &CLAUDE, until + 1);
        assert!(!later.blocked);
        assert_eq!(later.label, "週次の残り");
        // Fetched numbers wait for the next fetch to confirm the reset.
        let pending = super::hero(Some(&usage), &CODEX, until + 1);
        assert!(pending.blocked);
        assert_eq!(pending.value, "確認中…");
        assert_eq!(bar_text(&pending), "5時間の上限に達しました");
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
        assert_eq!(hero.when, "不明");
        assert_eq!(hero.detail.as_deref(), Some("314 / 500 クレジット使用"));
        let lines = lines(&usage, false, NOW);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            (lines[0].title.as_str(), lines[0].value.as_str()),
            ("クレジット残高", "1250")
        );
        assert_eq!(blocks(&usage, &CODEX, NOW), [hero]);
    }

    #[test]
    fn windows_that_reset_together_share_one_reset_line() {
        let usage = snapshot(
            vec![
                Group::new(Some("Gemini".into()), vec![window(10080, 100.0, Some(NOW))]),
                Group::new(
                    Some("Claude・GPT".into()),
                    vec![window(10080, 100.0, Some(NOW))],
                ),
            ],
            None,
        );
        let blocks = blocks(&usage, &CLAUDE, NOW - 60);
        assert!(shares_reset(&blocks[0], &blocks[1]));
        let apart = snapshot(
            vec![Group::new(
                None,
                vec![
                    window(300, 50.0, Some(NOW)),
                    window(10080, 50.0, Some(NOW + 60)),
                ],
            )],
            None,
        );
        let blocks = super::blocks(&apart, &CODEX, NOW - 60);
        assert!(!shares_reset(&blocks[0], &blocks[1]));
    }

    #[test]
    fn stale_and_missing_data_use_plain_words() {
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
        assert_eq!(hero(Some(&expired), &CODEX, NOW).when, "確認中…");
        assert_eq!(hero(Some(&expired), &CLAUDE, NOW).when, "リセット済み");
        assert_eq!(lines(&expired, false, NOW)[0].value, "確認中…");
        assert_eq!(lines(&expired, true, NOW)[0].value, "リセット済み");
        assert_eq!(blocks(&expired, &CLAUDE, NOW)[0].value, "—");
        assert_eq!(hero(None, &CLAUDE, NOW).when, "まだデータがありません");
        assert_eq!(hero(None, &CODEX, NOW).when, "確認中…");
        let unknown = snapshot(vec![Group::default()], None);
        assert_eq!(
            hero(Some(&unknown), &CODEX, NOW).when,
            "残量を確認できませんでした"
        );
    }

    #[test]
    fn waits_round_up_to_the_next_minute() {
        assert_eq!(duration_label(NOW + 1, NOW), "1分");
        assert_eq!(duration_label(NOW + 59 * 60 + 1, NOW), "1時間0分");
        assert_eq!(duration_label(NOW + 3 * 86_400 + 4 * 3600, NOW), "3日4時間");
    }
}
