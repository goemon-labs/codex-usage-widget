use crate::{
    codex::{self, Account, ErrorKind, FetchError},
    platform::{self, Action},
    quota::{Usage, Window},
    settings::Settings,
};
use chrono::{Datelike, Local, TimeZone};
use eframe::egui::{
    self, Align, Color32, FontData, FontDefinitions, FontFamily, Layout, RichText, Sense, Stroke,
    ViewportCommand, vec2,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const WIDTH: f32 = 252.0;
pub const HEIGHT: f32 = 320.0;
const BAR_WIDTH: f32 = 212.0;
const BAR_HEIGHT: f32 = 40.0;
const MENU_SPACE: f32 = 208.0;
const BACKGROUND: Color32 = Color32::from_rgb(20, 24, 29);
const FOREGROUND: Color32 = Color32::from_rgb(233, 239, 242);
const MUTED: Color32 = Color32::from_rgb(145, 157, 168);
const GREEN: Color32 = Color32::from_rgb(101, 220, 173);
const BORDER: Color32 = Color32::from_rgb(45, 53, 61);

pub fn window_size(bar_mode: bool) -> egui::Vec2 {
    if bar_mode {
        vec2(BAR_WIDTH, BAR_HEIGHT)
    } else {
        vec2(WIDTH, HEIGHT)
    }
}

enum FetchEvent {
    Detected(Option<codex::Installation>),
    Account(Account),
    Finished(Result<Usage, FetchError>),
}

struct Worker {
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

struct BarMenu {
    origin: Option<egui::Pos2>,
    above: bool,
}

pub struct Widget {
    settings: Settings,
    instance: Option<crate::instance::Instance>,
    installation: Option<codex::Installation>,
    usage: Option<Usage>,
    account: Option<Account>,
    error: Option<FetchError>,
    notice: Option<String>,
    worker: Option<Worker>,
    fetch_tx: Sender<FetchEvent>,
    fetch_rx: Receiver<FetchEvent>,
    action_tx: Sender<Action>,
    action_rx: Receiver<Action>,
    tray: Option<tray_icon::TrayIcon>,
    tray_attempted: bool,
    next_refresh: Option<i64>,
    last_started: Option<Instant>,
    failures: u32,
    settings_open: bool,
    auto_start: bool,
    hidden: bool,
    size: egui::Vec2,
    menu_open: bool,
    bar_menu: Option<BarMenu>,
    monitor_rect: Option<egui::Rect>,
    reset_details_open: bool,
    save_after: Option<Instant>,
}

impl Widget {
    pub fn new(ctx: &egui::Context, settings: Settings) -> Self {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "japanese".into(),
            Arc::new(FontData::from_static(include_bytes!(
                "../assets/WidgetSansJP.ttf"
            ))),
        );
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("japanese".into());
        }
        ctx.set_fonts(fonts);
        let mut style = egui::Style {
            visuals: egui::Visuals::dark(),
            ..Default::default()
        };
        style.visuals.override_text_color = Some(FOREGROUND);
        style.visuals.window_fill = BACKGROUND;
        style.visuals.panel_fill = BACKGROUND;
        style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(31, 37, 44);
        style.visuals.selection.bg_fill = Color32::from_rgb(47, 100, 83);
        style.animation_time = 0.0;
        style.interaction.selectable_labels = false;
        style.spacing.item_spacing = vec2(8.0, 5.0);
        style.spacing.button_padding = vec2(9.0, 5.0);
        ctx.set_theme(egui::Theme::Dark);
        ctx.set_style_of(egui::Theme::Dark, style);
        let (fetch_tx, fetch_rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let auto_start = platform::auto_launch()
            .and_then(|auto| auto.is_enabled().map_err(|error| error.to_string()))
            .unwrap_or(false);
        let size = window_size(settings.bar_mode);
        Self {
            settings,
            instance: None,
            installation: None,
            usage: None,
            account: None,
            error: None,
            notice: None,
            worker: None,
            fetch_tx,
            fetch_rx,
            action_tx,
            action_rx,
            tray: None,
            tray_attempted: false,
            next_refresh: Some(Local::now().timestamp()),
            last_started: None,
            failures: 0,
            settings_open: false,
            auto_start,
            hidden: false,
            size,
            menu_open: false,
            bar_menu: None,
            monitor_rect: None,
            reset_details_open: false,
            save_after: None,
        }
    }

    pub fn attach_instance(
        &mut self,
        ctx: &egui::Context,
        mut instance: crate::instance::Instance,
    ) -> std::io::Result<()> {
        let sender = self.action_tx.clone();
        let context = ctx.clone();
        instance.listen(move || {
            let _ = sender.send(Action::Show);
            context.request_repaint();
        })?;
        self.instance = Some(instance);
        Ok(())
    }

    fn refresh(&mut self, ctx: &egui::Context, changed_configuration: bool) {
        if self.worker.is_some() {
            return;
        }
        if !changed_configuration && let Some(started) = self.last_started {
            let elapsed = started.elapsed();
            if elapsed < Duration::from_secs(30) {
                self.next_refresh =
                    Some(Local::now().timestamp() + (30 - elapsed.as_secs()) as i64);
                return;
            }
        }
        let configured = self.settings.codex_path.clone();
        self.last_started = Some(Instant::now());
        self.next_refresh = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let sender = self.fetch_tx.clone();
        let context = ctx.clone();
        let handle = thread::spawn(move || {
            let installation = codex::find_codex(configured.as_deref());
            let _ = sender.send(FetchEvent::Detected(installation.clone()));
            context.request_repaint();
            let result = installation
                .ok_or_else(|| FetchError {
                    kind: ErrorKind::Setup,
                    message: codex::SETUP_GUIDANCE.into(),
                })
                .and_then(|installation| {
                    codex::fetch(&installation, &flag, |account| {
                        let _ = sender.send(FetchEvent::Account(account));
                        context.request_repaint();
                    })
                });
            let _ = sender.send(FetchEvent::Finished(result));
            context.request_repaint();
        });
        self.worker = Some(Worker { cancel, handle });
    }

    fn receive_results(&mut self) {
        while let Ok(event) = self.fetch_rx.try_recv() {
            match event {
                FetchEvent::Detected(installation) => self.installation = installation,
                FetchEvent::Account(account) => {
                    if self.account.as_ref() != Some(&account) {
                        self.usage = None;
                    }
                    self.account = Some(account);
                }
                FetchEvent::Finished(result) => {
                    if let Some(worker) = self.worker.take() {
                        let _ = worker.handle.join();
                    }
                    let now = Local::now().timestamp();
                    match result {
                        Ok(usage) => {
                            self.failures = 0;
                            self.error = None;
                            self.next_refresh = Some(
                                usage
                                    .next_reset(now)
                                    .map_or(now + 300, |reset| reset.min(now + 300)),
                            );
                            if let Some(tray) = &self.tray {
                                let text = usage
                                    .weekly
                                    .as_ref()
                                    .map_or("週次の情報を確認中".into(), |window| {
                                        format!("週次の残り {}", window.remaining_label())
                                    });
                                let _ = tray.set_tooltip(Some(format!("Codex · {text}")));
                            }
                            self.usage = Some(usage);
                        }
                        Err(error) => {
                            self.failures = self.failures.saturating_add(1);
                            self.next_refresh = match error.kind {
                                ErrorKind::Setup | ErrorKind::Login | ErrorKind::Cancelled => None,
                                _ => Some(now + if self.failures >= 3 { 900 } else { 300 }),
                            };
                            if error.kind == ErrorKind::Login {
                                self.usage = None;
                                self.account = None;
                            }
                            self.error = Some(error);
                        }
                    }
                }
            }
        }
    }

    fn show(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn hide(&mut self, ctx: &egui::Context) {
        if self.tray.is_some() {
            self.hidden = true;
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
    }

    fn persist(&mut self) {
        self.save_after = None;
        if self.settings.save().is_err() {
            self.notice =
                Some("設定を保存できませんでした。保存先へのアクセスを確認してください。".into());
        }
    }

    fn set_auto_start(&mut self, enabled: bool) {
        let result = platform::auto_launch().and_then(|auto| {
            (if enabled {
                auto.enable()
            } else {
                auto.disable()
            })
            .map_err(|error| error.to_string())
        });
        if result.is_ok() {
            self.auto_start = enabled;
        } else {
            self.notice = Some("自動起動の設定を変更できませんでした。".into());
        }
    }

    fn update_topmost(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(
            if self.settings.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            },
        ));
        self.persist();
    }

    fn set_bar_mode(&mut self, bar_mode: bool) {
        self.settings.bar_mode = bar_mode;
        self.settings_open = false;
        self.persist();
    }

    fn reset_connection(&mut self, ctx: &egui::Context) {
        self.usage = None;
        self.account = None;
        self.error = None;
        self.failures = 0;
        self.persist();
        self.refresh(ctx, true);
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if ui
            .checkbox(&mut self.settings.always_on_top, "最前面に表示")
            .changed()
        {
            self.update_topmost(&ctx);
        }
        let mut auto_start = self.auto_start;
        if ui
            .checkbox(&mut auto_start, "ログイン時に自動起動")
            .changed()
        {
            self.set_auto_start(auto_start);
        }
        ui.separator();
        if !self.settings_open && ui.button("設定").clicked() {
            self.settings_open = true;
            ui.close();
        }
        if (self.settings.bar_mode || self.settings_open) && ui.button("通常表示").clicked() {
            self.set_bar_mode(false);
            ui.close();
        }
        if (!self.settings.bar_mode || self.settings_open) && ui.button("バーモード").clicked()
        {
            self.set_bar_mode(true);
            ui.close();
        }
        if ui
            .add_enabled(self.tray.is_some(), egui::Button::new("隠す"))
            .clicked()
        {
            self.hide(&ctx);
            ui.close();
        }
        if ui.button("終了").clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn menu_button(&mut self, ui: &mut egui::Ui, compact: bool) {
        let response = ui.add(
            egui::Button::new(RichText::new("⋯").size(18.0).color(MUTED)).selected(self.menu_open),
        );
        if response.clicked() {
            self.menu_open = !self.menu_open;
            if compact && self.menu_open {
                let origin = ui.input(|input| input.viewport().outer_rect.map(|rect| rect.min));
                let above = origin
                    .zip(self.monitor_rect)
                    .is_some_and(|(origin, screen)| {
                        let below = screen.bottom() - origin.y - BAR_HEIGHT;
                        origin.y - screen.top() > below
                    });
                self.bar_menu = Some(BarMenu { origin, above });
            }
        }
        // Wait for the transparent drawing area to grow before showing the bar's popup.
        if compact && ui.ctx().viewport_rect().height() + 1.0 < BAR_HEIGHT + MENU_SPACE {
            return;
        }
        let mut open = self.menu_open;
        let above = compact && self.bar_menu.as_ref().is_some_and(|menu| menu.above);
        egui::Popup::menu(&response)
            .open_bool(&mut open)
            .align(if above {
                egui::RectAlign::TOP_END
            } else {
                egui::RectAlign::BOTTOM_END
            })
            .align_alternatives(&[])
            .gap(if compact { 8.0 } else { 0.0 })
            .show(|ui| self.menu(ui));
        self.menu_open = open;
    }

    fn header(&mut self, ui: &mut egui::Ui, compact: bool) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(7.0, 24.0), Sense::hover());
            let weekly = self.usage.as_ref().and_then(|usage| usage.weekly.as_ref());
            let current = weekly.filter(|window| !window.expired(Local::now().timestamp()));
            let dot = if compact && self.error.is_some() {
                Color32::from_rgb(225, 180, 107)
            } else if compact && current.and_then(|window| window.remaining).is_none() {
                MUTED
            } else {
                GREEN
            };
            ui.painter().circle_filled(rect.center(), 2.5, dot);
            if compact {
                let remaining = current.map_or("—".into(), Window::remaining_label);
                ui.label(RichText::new(format!("週次の残り {remaining}")).size(13.0));
            } else {
                ui.label(RichText::new("Codex").size(14.0).strong());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.scope(|ui| {
                    ui.spacing_mut().button_padding = vec2(4.0, 1.0);
                    ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                    self.menu_button(ui, compact);
                });
            });
        });
    }

    fn usage_ui(&mut self, ui: &mut egui::Ui) {
        let now = Local::now().timestamp();
        ui.add_space(17.0);
        ui.label(RichText::new("週次の残り").size(11.0).color(MUTED));
        ui.add_space(1.0);
        let weekly = self.usage.as_ref().and_then(|usage| usage.weekly.as_ref());
        let expired = weekly.is_some_and(|window| window.expired(now));
        let label = if expired {
            "—".into()
        } else {
            weekly.map_or("—".into(), Window::remaining_label)
        };
        ui.label(RichText::new(label).size(32.0).color(FOREGROUND));
        ui.add_space(7.0);
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
        ui.painter().rect_filled(bar, 2.0, BORDER);
        if !expired && let Some(remaining) = weekly.and_then(|window| window.remaining) {
            let filled = egui::Rect::from_min_size(
                bar.min,
                vec2(bar.width() * remaining as f32 / 100.0, bar.height()),
            );
            if remaining > 0.0 {
                ui.painter().rect_filled(filled, 2.0, GREEN);
            }
        }
        ui.add_space(12.0);
        ui.label(RichText::new("リセット").size(11.0).color(MUTED));
        let reset = if expired {
            "リセット後の情報を確認中".into()
        } else if let Some(reset) = weekly.and_then(|window| window.resets_at) {
            reset_label(reset)
        } else if self.worker.is_some() && self.usage.is_none() {
            "取得中…".into()
        } else if weekly.is_some() {
            "リセット日時を確認できませんでした".into()
        } else if self.usage.is_some() {
            "週次の情報を取得できませんでした".into()
        } else {
            "Codexの利用枠を確認します".into()
        };
        ui.label(RichText::new(reset).size(12.0));
        ui.add_space(21.0);
        self.reset_credits_ui(ui, now);

        if let Some(short) = self.usage.as_ref().and_then(|usage| usage.short.as_ref()) {
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{}の残り", short.label()))
                        .size(11.0)
                        .color(MUTED),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(if short.expired(now) {
                            "確認中".into()
                        } else {
                            short.remaining_label()
                        })
                        .size(12.0),
                    );
                });
            });
            if let Some(reset) = short.resets_at {
                ui.label(RichText::new(reset_label(reset)).size(11.0).color(MUTED));
            }
        }
        if let Some(error) = &self.error {
            ui.add_space(15.0);
            ui.label(RichText::new(&error.message).size(11.0).color(MUTED));
            if matches!(error.kind, ErrorKind::Setup | ErrorKind::Login)
                && ui.small_button("設定を開く").clicked()
            {
                self.settings_open = true;
            }
        }

        // Keep the footer in the content flow so expanded details cannot overlap it.
        let used_height = ui.cursor().top() - ui.min_rect().top();
        ui.add_space((HEIGHT - 42.0 - used_height - 24.0).max(22.0));
        self.footer_ui(ui);
    }

    fn reset_credits_ui(&mut self, ui: &mut egui::Ui, now: i64) {
        let resets = self
            .usage
            .as_ref()
            .and_then(|usage| usage.reset_credits.as_ref());
        let count = resets.map_or("—".into(), |resets| {
            format!("{}枚", resets.available_count)
        });
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("利用制限のリセット {count}"),
            )
        });
        if response.clicked() {
            self.reset_details_open = !self.reset_details_open;
        }
        let color = if response.hovered() {
            FOREGROUND
        } else {
            MUTED
        };
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            "利用制限のリセット",
            egui::FontId::proportional(12.0),
            FOREGROUND,
        );
        ui.painter().text(
            rect.right_center() - vec2(18.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            count,
            egui::FontId::proportional(11.0),
            MUTED,
        );
        let center = rect.right_center() - vec2(4.0, 0.0);
        let direction = if self.reset_details_open { -1.0 } else { 1.0 };
        ui.painter().add(egui::Shape::line(
            vec![
                center + vec2(-3.0, -1.5 * direction),
                center + vec2(0.0, 1.5 * direction),
                center + vec2(3.0, -1.5 * direction),
            ],
            Stroke::new(1.2, color),
        ));
        response.on_hover_cursor(egui::CursorIcon::PointingHand);

        if self.reset_details_open {
            egui::ScrollArea::vertical()
                .id_salt("reset_credit_list")
                .max_height(160.0)
                .auto_shrink([false, true])
                .scroll_source(egui::scroll_area::ScrollSource {
                    drag: egui::scroll_area::DragScroll::Never,
                    ..Default::default()
                })
                .show(ui, |ui| {
                    if let Some(resets) = resets {
                        if resets.available_count == 0 {
                            ui.label(RichText::new("チケットは0枚です").size(11.0).color(MUTED));
                        } else if let Some(credits) = &resets.credits {
                            ui.label(RichText::new("有効期限").size(11.0).color(MUTED));
                            for (index, credit) in credits.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        vec2(14.0, 23.0),
                                        egui::Label::new(
                                            RichText::new(format!("{}", index + 1))
                                                .size(10.0)
                                                .color(MUTED),
                                        ),
                                    );
                                    ui.label(
                                        RichText::new(credit_expiry_label(credit.expires_at, now))
                                            .size(12.0),
                                    );
                                });
                            }
                        }
                        if !resets.details_complete() && resets.available_count > 0 {
                            ui.label(
                                RichText::new("一部の有効期限を確認中")
                                    .size(11.0)
                                    .color(MUTED),
                            );
                        }
                    } else {
                        ui.label(
                            RichText::new("チケット情報を確認中")
                                .size(11.0)
                                .color(MUTED),
                        );
                    }
                });
        } else {
            let first = resets
                .and_then(|resets| resets.credits.as_ref())
                .and_then(|credits| credits.first());
            let label = if resets.is_some_and(|resets| resets.available_count == 0) {
                "チケットは0枚です".into()
            } else if let Some(credit) = first {
                let caption = if resets.is_some_and(|resets| resets.details_complete()) {
                    "最短"
                } else {
                    "確認済み"
                };
                format!("{caption}  {}", credit_expiry_label(credit.expires_at, now))
            } else if self.worker.is_some() && self.usage.is_none() {
                "取得中…".into()
            } else {
                "有効期限を確認中".into()
            };
            ui.label(RichText::new(label).size(11.0).color(MUTED));
        }
    }

    fn footer_ui(&mut self, ui: &mut egui::Ui) {
        let fetched = self.usage.as_ref().map(|usage| {
            let format = if usage.fetched_at.date_naive() == Local::now().date_naive() {
                "%H:%M"
            } else {
                "%-m/%-d %H:%M"
            };
            usage.fetched_at.format(format).to_string()
        });
        let status = if self.worker.is_some() {
            "更新中…".into()
        } else if let Some(time) = fetched {
            format!(
                "{} {time}",
                if self.error.is_some() {
                    "前回取得"
                } else {
                    "更新"
                }
            )
        } else {
            "5分ごとに更新".into()
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(status).size(10.0).color(MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let enabled = self.worker.is_none()
                    && self
                        .last_started
                        .is_none_or(|started| started.elapsed() >= Duration::from_secs(30));
                let response = ui
                    .add_enabled_ui(enabled, |ui| {
                        let (rect, response) =
                            ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                enabled,
                                "今すぐ更新",
                            )
                        });
                        let color = if !enabled {
                            MUTED.gamma_multiply(0.5)
                        } else if response.hovered() {
                            FOREGROUND
                        } else {
                            MUTED
                        };
                        let center = rect.center();
                        let points: Vec<_> = (0..=20)
                            .map(|step| {
                                let angle = (45.0 + step as f32 * 270.0 / 20.0).to_radians();
                                center + vec2(angle.cos(), angle.sin()) * 5.0
                            })
                            .collect();
                        let tip = *points.last().expect("arc has points");
                        ui.painter()
                            .add(egui::Shape::line(points, Stroke::new(1.3, color)));
                        ui.painter().add(egui::Shape::line(
                            vec![tip + vec2(-3.5, 0.0), tip, tip + vec2(0.0, -3.5)],
                            Stroke::new(1.3, color),
                        ));
                        response
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text("今すぐ更新")
                    })
                    .inner;
                if response.clicked() {
                    self.refresh(ui.ctx(), false);
                }
            });
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(17.0);
        ui.label(RichText::new("Codexとの接続").strong());
        ui.label(
            RichText::new(match &self.installation {
                Some(installation) if installation.is_app => "Codexアプリを検出しました",
                Some(_) => "Codex CLIを検出しました",
                None if self.worker.is_some() => "Codexを探しています…",
                None => "CodexアプリまたはCLIが必要です",
            })
            .size(12.0)
            .color(MUTED),
        );
        if let Some(installation) = &self.installation {
            let path = &installation.path;
            ui.add(
                egui::Label::new(
                    RichText::new(path.to_string_lossy())
                        .size(10.0)
                        .color(MUTED),
                )
                .truncate(),
            )
            .on_hover_text(path.to_string_lossy());
        }
        ui.add_enabled_ui(self.worker.is_none(), |ui| {
            ui.horizontal(|ui| {
                if ui.button("場所を選択").clicked() {
                    let dialog = rfd::FileDialog::new().set_title("公式Codexの実行ファイルを選択");
                    #[cfg(windows)]
                    let dialog = dialog.add_filter("Codex", &["exe", "cmd"]);
                    if let Some(path) = dialog.pick_file() {
                        self.settings.codex_path = Some(path);
                        self.reset_connection(ui.ctx());
                    }
                }
                if ui.button("自動検出").clicked() {
                    self.settings.codex_path = None;
                    self.reset_connection(ui.ctx());
                }
            });
        });
        ui.add_space(5.0);
        ui.label(
            RichText::new(
                "公式CodexのアプリまたはCLIに、ChatGPTアカウントでログインしてください。",
            )
            .size(11.0)
            .color(MUTED),
        );
        ui.hyperlink_to(
            "公式の導入・ログイン手順",
            "https://learn.chatgpt.com/docs/quickstart?setup=app",
        );
        if let Some(account) = &self.account {
            ui.add_space(5.0);
            if let Some(email) = &account.email {
                ui.add(egui::Label::new(RichText::new(email).size(11.0)).truncate());
            }
            if let Some(plan) = &account.plan {
                ui.label(
                    RichText::new(format!("プラン：{plan}"))
                        .size(11.0)
                        .color(MUTED),
                );
            }
        }
        if ui
            .add_enabled(self.worker.is_none(), egui::Button::new("接続を確認"))
            .clicked()
        {
            self.refresh(ui.ctx(), false);
        }
        if let Some(error) = &self.error {
            ui.label(RichText::new(&error.message).size(11.0).color(MUTED));
        }
        if let Some(notice) = &self.notice {
            ui.label(RichText::new(notice).size(11.0).color(MUTED));
        }
        ui.add_space(18.0);
        ui.label(
            RichText::new(format!("Codex Usage Widget  {}", env!("CARGO_PKG_VERSION")))
                .size(10.0)
                .color(MUTED),
        );
    }
}

impl Widget {
    fn render(&mut self, ui: &mut egui::Ui) {
        let compact = self.settings.bar_mode && !self.settings_open;
        let size = window_size(compact);
        let margin = if compact {
            egui::Margin::symmetric(12, 7)
        } else {
            egui::Margin::same(20)
        };
        if compact && self.bar_menu.as_ref().is_some_and(|menu| menu.above) {
            ui.add_space(MENU_SPACE);
        }
        // Register the background first so buttons and other controls keep their clicks.
        let drag_size = vec2(
            size.x,
            if compact {
                BAR_HEIGHT
            } else {
                self.size.y.max(size.y)
            },
        );
        let card_rect = egui::Rect::from_min_size(ui.cursor().min, drag_size);
        let drag = ui.interact(card_rect, ui.id().with("window_drag"), Sense::click());
        if !self.menu_open
            && drag.is_pointer_button_down_on()
            && ui.input(|input| input.pointer.primary_pressed())
        {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        let frame = egui::Frame::new()
            .fill(BACKGROUND)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(14)
            .inner_margin(margin)
            .show(ui, |ui| {
                // Use the requested width immediately when switching modes.
                ui.set_width(size.x - margin.sum().x - 2.0);
                ui.set_min_height(size.y - margin.sum().y - 2.0);
                self.header(ui, compact);
                if !compact {
                    if self.settings_open {
                        self.settings_ui(ui);
                    } else {
                        self.usage_ui(ui);
                    }
                }
            });
        let expanded = compact && self.menu_open && !self.settings_open;
        let size = vec2(
            size.x,
            if expanded {
                BAR_HEIGHT + MENU_SPACE
            } else {
                frame.response.rect.height().ceil()
            },
        );
        if expanded {
            if let Some(menu) = &self.bar_menu
                && menu.above
                && let Some(origin) = menu.origin
            {
                let position = origin - vec2(0.0, MENU_SPACE);
                if ui.input(|input| input.viewport().outer_rect.map(|rect| rect.min))
                    != Some(position)
                {
                    ui.ctx()
                        .send_viewport_cmd(ViewportCommand::OuterPosition(position));
                }
            }
        } else if let Some(menu) = self.bar_menu.take()
            && let Some(origin) = menu.origin
        {
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::OuterPosition(origin));
        }
        if self.size != size {
            self.size = size;
            ui.ctx().send_viewport_cmd(ViewportCommand::InnerSize(size));
            ui.ctx().request_repaint();
        }
    }
}

impl eframe::App for Widget {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        // This widget only uploads text. Avoid sizing its atlas for the GPU's maximum texture width.
        input.max_texture_side = Some(input.max_texture_side.unwrap_or(2048).min(2048));
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.tray_attempted {
            self.tray_attempted = true;
            match platform::create_tray(self.action_tx.clone(), ctx) {
                Ok(tray) => self.tray = Some(tray),
                Err(_) => {
                    self.notice = Some(
                        "常駐アイコンを作成できませんでした。画面のメニューから操作できます。"
                            .into(),
                    )
                }
            }
        }
        while let Ok(action) = self.action_rx.try_recv() {
            match action {
                Action::Show => self.show(ctx),
                Action::Refresh => self.refresh(ctx, false),
                Action::Settings => {
                    self.settings_open = true;
                    self.show(ctx);
                }
                Action::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            }
        }
        self.receive_results();
        let now = Local::now().timestamp();
        if self.next_refresh.is_some_and(|deadline| now >= deadline) {
            self.refresh(ctx, false);
        }
        if !self.hidden
            && self.bar_menu.is_none()
            && let Some(rect) = ctx.input(|input| input.viewport().outer_rect)
        {
            let position = [rect.min.x, rect.min.y];
            if position.iter().all(|value| value.is_finite())
                && self.settings.position != Some(position)
            {
                self.settings.position = Some(position);
                self.save_after = Some(Instant::now() + Duration::from_secs(1));
            }
        }
        if let Some(deadline) = self.save_after {
            if Instant::now() >= deadline {
                self.persist();
            } else {
                ctx.request_repaint_after(deadline.saturating_duration_since(Instant::now()));
            }
        }
        // A low-frequency clock check also catches sleep/resume and clock changes, including while hidden.
        let wait = self
            .next_refresh
            .map_or(30, |deadline| (deadline - now).clamp(1, 30));
        ctx.request_repaint_after(Duration::from_secs(wait as u64));
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.settings.bar_mode
            && ui.input(|input| {
                input.pointer.primary_pressed()
                    || input.key_pressed(egui::Key::Enter)
                    || input.key_pressed(egui::Key::Space)
            })
        {
            self.monitor_rect = frame
                .winit_window()
                .and_then(|window| platform::monitor_rect(window));
        }
        if !ui.input(|input| input.focused) {
            self.menu_open = false;
        }
        self.render(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }
}

impl Drop for Widget {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.cancel.store(true, Ordering::Relaxed);
            let _ = worker.handle.join();
        }
        self.persist();
    }
}

fn reset_label(timestamp: i64) -> String {
    Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map_or("確認中".into(), |date| {
            let weekday = ["月", "火", "水", "木", "金", "土", "日"]
                [date.weekday().num_days_from_monday() as usize];
            format!(
                "{}月{}日（{}）{}",
                date.month(),
                date.day(),
                weekday,
                date.format("%H:%M")
            )
        })
}

fn credit_expiry_label(expires_at: Option<i64>, now: i64) -> String {
    match expires_at {
        Some(expiry) if expiry <= now => "期限切れ・更新待ち".into(),
        Some(expiry) => reset_label(expiry),
        None => "有効期限なし".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_covers_card_text_and_margins_but_preserves_control_clicks() {
        let ctx = egui::Context::default();
        // Render in memory without starting the worker or saving user settings.
        let mut widget = std::mem::ManuallyDrop::new(Widget::new(&ctx, Settings::default()));
        let step = |widget: &mut Widget, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, widget.size)),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ui| widget.render(ui),
            )
        };
        for _ in 0..2 {
            step(&mut widget, vec![]).drop_without_applying_deltas();
        }
        let footer_y = widget.size.y - 33.0;
        for (x, y, should_drag) in [
            (12.0, 12.0, true),
            (48.0, 33.0, true),
            (55.0, 105.0, true),
            (85.0, 181.0, true),
            (218.0, 33.0, false),
            (110.0, 225.0, false),
            (218.0, footer_y, false),
        ] {
            widget.last_started = None;
            step(&mut widget, vec![]).drop_without_applying_deltas();
            let pos = egui::pos2(x, y);
            let output = step(
                &mut widget,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            let starts_drag = output.viewport_output.values().any(|viewport| {
                viewport
                    .commands
                    .iter()
                    .any(|command| matches!(command, ViewportCommand::StartDrag))
            });
            output.drop_without_applying_deltas();
            assert_eq!(starts_drag, should_drag, "pointer at ({x}, {y})");
            widget.last_started = Some(Instant::now());
            let outside = egui::pos2(-40.0, -40.0);
            step(
                &mut widget,
                vec![
                    egui::Event::PointerMoved(outside),
                    egui::Event::PointerButton {
                        pos: outside,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .drop_without_applying_deltas();
        }
        assert!(widget.worker.is_none());
    }

    #[test]
    fn bar_stays_one_line_with_a_clickable_menu_and_returns_from_settings() {
        let ctx = egui::Context::default();
        let mut widget = std::mem::ManuallyDrop::new(Widget::new(
            &ctx,
            Settings {
                bar_mode: true,
                ..Default::default()
            },
        ));
        let step = |widget: &mut Widget, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, widget.size)),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ui| widget.render(ui),
            )
        };
        for (remaining, expected) in [(None, "—"), (Some(100.0), "100%"), (Some(0.5), "1%未満")]
        {
            widget.usage = Some(Usage {
                weekly: Some(Window {
                    minutes: 10080,
                    remaining,
                    resets_at: None,
                }),
                short: None,
                reset_credits: None,
                fetched_at: Local::now(),
            });
            step(&mut widget, vec![]).drop_without_applying_deltas();
            let output = step(&mut widget, vec![]);
            let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, window_size(true));
            let texts: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape {
                        assert!(bounds.contains_rect(shape.shape.visual_bounding_rect()));
                        Some(text.galley.job.text.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(texts, [format!("週次の残り {expected}"), "⋯".into()]);
            assert_eq!(widget.size, window_size(true));
            output.drop_without_applying_deltas();
        }
        for (pos, should_drag) in [
            (egui::pos2(60.0, 20.0), true),
            (egui::pos2(187.0, 20.0), false),
        ] {
            let output = step(
                &mut widget,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert_eq!(
                output
                    .viewport_output
                    .values()
                    .any(|viewport| viewport.commands.contains(&ViewportCommand::StartDrag)),
                should_drag
            );
            output.drop_without_applying_deltas();
            step(
                &mut widget,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            )
            .drop_without_applying_deltas();
        }
        assert!(widget.menu_open);
        for _ in 0..2 {
            step(&mut widget, vec![]).drop_without_applying_deltas();
        }
        let output = step(&mut widget, vec![]);
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, widget.size);
        let mut menu_visible = false;
        for shape in &output.shapes {
            if let egui::Shape::Text(text) = &shape.shape {
                assert!(bounds.contains_rect(shape.shape.visual_bounding_rect()));
                menu_visible |= text.galley.job.text == "通常表示";
                assert_ne!(text.galley.job.text, "バーモード");
            }
        }
        assert!(menu_visible);
        output.drop_without_applying_deltas();
        step(
            &mut widget,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
        .drop_without_applying_deltas();
        assert!(!widget.menu_open);
        assert_eq!(widget.size, window_size(true));
        widget.settings_open = true;
        for _ in 0..2 {
            step(&mut widget, vec![]).drop_without_applying_deltas();
        }
        assert_eq!(widget.size.x, WIDTH);
        assert!(widget.size.y >= HEIGHT);
        let output = step(&mut widget, vec![]);
        for shape in &output.shapes {
            if let egui::Shape::Text(text) = &shape.shape {
                assert!(
                    !["設定", "戻る", "最前面に表示", "ログイン時に自動起動"]
                        .contains(&text.galley.job.text.as_str())
                );
            }
        }
        output.drop_without_applying_deltas();
        widget.settings_open = false;
        for _ in 0..2 {
            step(&mut widget, vec![]).drop_without_applying_deltas();
        }
        assert_eq!(widget.size, window_size(true));
        assert!(widget.worker.is_none());
    }
}
