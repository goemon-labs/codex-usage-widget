mod card;

use crate::{
    bridge,
    platform::{self, Action},
    quota::Snapshot,
    services::{
        self, ServiceId,
        codex::{self, Account, ErrorKind, FetchError},
    },
    settings::{self, Settings},
};
use chrono::{DateTime, Datelike, Local, TimeZone};
use eframe::egui::{
    self, Align, Color32, FontData, FontDefinitions, FontFamily, Layout, RichText, Sense, Stroke,
    ViewportCommand, vec2,
};
use std::{
    collections::BTreeMap,
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

pub const WIDTH: f32 = 252.0;
pub const HEIGHT: f32 = 320.0;
const BAR_WIDTH: f32 = 212.0;
const BAR_HEIGHT: f32 = 40.0;
/// Size of a service icon in the bar, matching its 13px text.
const ICON: f32 = 14.0;
const MENU_SPACE: f32 = 208.0;
const BACKGROUND: Color32 = Color32::from_rgb(20, 24, 29);
const FOREGROUND: Color32 = Color32::from_rgb(233, 239, 242);
const MUTED: Color32 = Color32::from_rgb(145, 157, 168);
const GREEN: Color32 = Color32::from_rgb(101, 220, 173);
const AMBER: Color32 = Color32::from_rgb(225, 180, 107);
const BORDER: Color32 = Color32::from_rgb(45, 53, 61);

pub fn window_size(bar_mode: bool) -> egui::Vec2 {
    if bar_mode {
        vec2(BAR_WIDTH, BAR_HEIGHT)
    } else {
        vec2(WIDTH, HEIGHT)
    }
}

enum FetchEvent {
    Account(Account),
    Finished(Result<Snapshot, FetchError>),
    Received(ServiceId, Snapshot),
}

struct Worker {
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

/// Picks up usage that tools store while they run, without waking the window.
struct Watcher {
    services: Vec<ServiceId>,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl Watcher {
    fn start(services: Vec<ServiceId>, sender: Sender<FetchEvent>, context: egui::Context) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let watched = services.clone();
        let handle = thread::spawn(move || {
            let mut seen: BTreeMap<ServiceId, Option<SystemTime>> = BTreeMap::new();
            while !flag.load(Ordering::Relaxed) {
                for &service in &watched {
                    let Some(path) = bridge::received_path(service) else {
                        continue;
                    };
                    let modified = fs::metadata(&path).and_then(|data| data.modified()).ok();
                    if seen.insert(service, modified) == Some(modified) {
                        continue;
                    }
                    let now = Local::now();
                    if let Some((received_at, data)) = bridge::read(&path)
                        && let Some(snapshot) = services::received_snapshot(
                            service,
                            &data,
                            Local.timestamp_opt(received_at, 0).single().unwrap_or(now),
                            now.timestamp(),
                        )
                    {
                        let _ = sender.send(FetchEvent::Received(service, snapshot));
                        context.request_repaint();
                    }
                }
                thread::park_timeout(Duration::from_secs(3));
            }
        });
        Self {
            services,
            stop,
            handle,
        }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.thread().unpark();
        let _ = self.handle.join();
    }
}

struct BarMenu {
    origin: Option<egui::Pos2>,
    above: bool,
}

pub struct Widget {
    settings: Settings,
    #[cfg(windows)]
    topmost: Option<platform::topmost::Topmost>,
    instance: Option<crate::instance::Instance>,
    /// Codex usage from the latest fetch.
    usage: Option<Snapshot>,
    /// Usage that other tools reported while running.
    received: BTreeMap<ServiceId, Snapshot>,
    account: Option<Account>,
    error: Option<FetchError>,
    notice: Option<String>,
    worker: Option<Worker>,
    watcher: Option<Watcher>,
    /// A service whose registration failed, offered for manual setup.
    failed_link: Option<ServiceId>,
    /// The service opened from the combined view.
    detail: Option<ServiceId>,
    /// Logos the user placed in the icons folder, shown in the bar instead of names.
    icons: BTreeMap<ServiceId, egui::TextureHandle>,
    /// Services found on the first launch, waiting for the user to pick what to show.
    choices: Option<Vec<ServiceId>>,
    started: bool,
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
            #[cfg(windows)]
            topmost: None,
            instance: None,
            usage: None,
            received: BTreeMap::new(),
            account: None,
            error: None,
            notice: None,
            worker: None,
            watcher: None,
            failed_link: None,
            detail: None,
            icons: BTreeMap::new(),
            choices: None,
            started: false,
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

    #[cfg(windows)]
    pub fn attach_window(&mut self, window: &winit::window::Window) {
        self.topmost = platform::topmost::Topmost::new(window, self.settings.always_on_top);
    }

    fn shows(&self, service: ServiceId) -> bool {
        self.settings.services.contains(&service)
    }

    fn snapshot(&self, service: ServiceId) -> Option<&Snapshot> {
        match service {
            ServiceId::Codex => self.usage.as_ref(),
            _ => self.received.get(&service),
        }
    }

    fn source(&self, service: ServiceId) -> card::Source {
        card::Source {
            name: service.name(),
            received: service.received(),
            loading: service == ServiceId::Codex && self.worker.is_some() && self.usage.is_none(),
        }
    }

    fn update_tray(&self) {
        let Some(tray) = &self.tray else {
            return;
        };
        let now = Local::now().timestamp();
        let text: Vec<_> = self
            .settings
            .services
            .iter()
            .map(|&service| {
                let hero = card::hero(self.snapshot(service), &self.source(service), now);
                format!("{} · {}", service.name(), card::bar_text(&hero))
            })
            .collect();
        let _ = tray.set_tooltip(Some(text.join("\n")));
    }

    /// Watch exactly the selected services that report usage on their own.
    fn sync_watcher(&mut self, ctx: &egui::Context) {
        let wanted: Vec<_> = self
            .settings
            .services
            .iter()
            .copied()
            .filter(|service| service.received())
            .collect();
        if self.watcher.as_ref().map(|watcher| &watcher.services) == Some(&wanted) {
            return;
        }
        if let Some(watcher) = self.watcher.take() {
            watcher.stop();
        }
        if !wanted.is_empty() {
            self.watcher = Some(Watcher::start(wanted, self.fetch_tx.clone(), ctx.clone()));
        }
    }

    fn refresh(&mut self, ctx: &egui::Context, changed_configuration: bool) {
        if !self.shows(ServiceId::Codex) {
            self.next_refresh = None;
            return;
        }
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
                            self.usage = Some(usage);
                            self.update_tray();
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
                FetchEvent::Received(service, snapshot) => {
                    self.received.insert(service, snapshot);
                    self.update_tray();
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
        #[cfg(windows)]
        if let Some(topmost) = &self.topmost {
            topmost.set_enabled(self.settings.always_on_top);
        }
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

    /// The service whose card is shown, or `None` for the combined view.
    fn shown(&self) -> Option<ServiceId> {
        match self.settings.services.as_slice() {
            [only] => Some(*only),
            _ => self.detail,
        }
    }

    fn heroes(&self, now: i64) -> Vec<(ServiceId, card::Hero)> {
        self.settings
            .services
            .iter()
            .map(|&service| {
                let hero = card::hero(self.snapshot(service), &self.source(service), now);
                (service, hero)
            })
            .collect()
    }

    /// What the bar shows for each service: its icon or name, then what is left.
    fn bar_items(&self, now: i64) -> Vec<(Option<egui::TextureId>, String)> {
        match self.heroes(now).as_slice() {
            [(_, hero)] => vec![(None, card::bar_text(hero))],
            heroes => heroes
                .iter()
                .map(|(service, hero)| match self.icons.get(service) {
                    Some(icon) => (Some(icon.id()), hero.short_value.clone()),
                    None => (
                        None,
                        format!("{} {}", service.short_name(), hero.short_value),
                    ),
                })
                .collect(),
        }
    }

    /// The bar grows to fit every selected service.
    fn bar_width(&self, ui: &egui::Ui) -> f32 {
        let items = self.bar_items(Local::now().timestamp());
        let content: f32 = items
            .iter()
            .map(|(icon, text)| {
                let text = ui
                    .painter()
                    .layout_no_wrap(text.clone(), egui::FontId::proportional(13.0), FOREGROUND)
                    .size()
                    .x;
                text + if icon.is_some() { ICON + 4.0 } else { 0.0 }
            })
            .sum::<f32>()
            + 12.0 * items.len().saturating_sub(1) as f32;
        // The status dot, gaps, menu button and margins around the content.
        (content + 88.0).ceil().max(BAR_WIDTH)
    }

    fn bar_ui(&self, ui: &mut egui::Ui, now: i64) {
        for (index, (icon, text)) in self.bar_items(now).into_iter().enumerate() {
            if index > 0 {
                ui.add_space(4.0);
            }
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if let Some(icon) = icon {
                    ui.add(egui::Image::new((icon, vec2(ICON, ICON))));
                }
                ui.label(RichText::new(text).size(13.0));
            });
        }
    }

    /// Load the logos the user placed in the icons folder; missing ones fall back to names.
    fn load_icons(&mut self, ctx: &egui::Context) {
        let Some(directory) = settings::icons_dir() else {
            return;
        };
        for service in ServiceId::ALL {
            let Ok(bytes) = fs::read(directory.join(format!("{}.png", service.key()))) else {
                continue;
            };
            // Keep the texture small enough for any GPU; icons are drawn at 14px anyway.
            let Some(icon) = eframe::icon_data::from_png_bytes(&bytes)
                .ok()
                .filter(|icon| icon.width <= 1024 && icon.height <= 1024)
            else {
                continue;
            };
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            let texture = ctx.load_texture(
                format!("icon-{}", service.key()),
                image,
                egui::TextureOptions::LINEAR,
            );
            self.icons.insert(service, texture);
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, compact: bool) {
        let now = Local::now().timestamp();
        let heroes = self.heroes(now);
        let shown = self.shown();
        ui.horizontal(|ui| {
            if !compact && !self.settings_open && self.detail.is_some() && shown.is_some() {
                self.back_button(ui);
            } else {
                let (rect, _) = ui.allocate_exact_size(vec2(7.0, 24.0), Sense::hover());
                let failed = self.shows(ServiceId::Codex) && self.error.is_some();
                let dot = if heroes.iter().any(|(_, hero)| hero.blocked) || (compact && failed) {
                    AMBER
                } else if compact && heroes.iter().all(|(_, hero)| hero.fraction.is_none()) {
                    MUTED
                } else {
                    GREEN
                };
                ui.painter().circle_filled(rect.center(), 2.5, dot);
            }
            if compact {
                self.bar_ui(ui, now);
            } else {
                let title = if self.settings_open {
                    "設定"
                } else {
                    shown.map_or("利用状況", ServiceId::name)
                };
                ui.label(RichText::new(title).size(14.0).strong());
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

    fn back_button(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(vec2(14.0, 24.0), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "まとめ表示に戻る")
        });
        let color = if response.hovered() {
            FOREGROUND
        } else {
            MUTED
        };
        let center = rect.center();
        ui.painter().add(egui::Shape::line(
            vec![
                center + vec2(2.5, -5.0),
                center + vec2(-2.5, 0.0),
                center + vec2(2.5, 5.0),
            ],
            Stroke::new(1.4, color),
        ));
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.detail = None;
        }
    }

    /// Every selected service in one card, each in its own section.
    fn combined_ui(&mut self, ui: &mut egui::Ui) {
        let now = Local::now().timestamp();
        let services = self.settings.services.clone();
        for (index, &service) in services.iter().enumerate() {
            ui.add_space(if index == 0 { 14.0 } else { 22.0 });
            self.section_header(ui, service);
            let source = self.source(service);
            match self.snapshot(service) {
                Some(snapshot) => {
                    let blocks = card::blocks(snapshot, &source, now);
                    if blocks.is_empty() {
                        ui.label(
                            RichText::new("利用枠の情報を取得できませんでした")
                                .size(12.0)
                                .color(MUTED),
                        );
                    }
                    card::stack_ui(ui, &blocks, 6.0);
                }
                None => {
                    let waiting = card::hero(None, &source, now).when;
                    ui.label(RichText::new(waiting).size(12.0).color(MUTED));
                }
            }
            if service == ServiceId::Codex
                && let Some(error) = &self.error
            {
                ui.label(RichText::new(&error.message).size(11.0).color(MUTED));
            } else if service.received() && !self.settings.bridges.contains_key(&service) {
                ui.label(RichText::new("連携していません").size(12.0).color(MUTED));
            }
        }
        let used_height = ui.cursor().top() - ui.min_rect().top();
        ui.add_space((HEIGHT - 42.0 - used_height - 24.0).max(16.0));
        self.footer_ui(ui, &services);
    }

    /// The service name opens its own card.
    fn section_header(&mut self, ui: &mut egui::Ui, service: ServiceId) {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("{}の詳細", service.name()),
            )
        });
        let painter = ui.painter();
        painter.text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            service.name(),
            egui::FontId::proportional(14.0),
            FOREGROUND,
        );
        let color = if response.hovered() {
            FOREGROUND
        } else {
            MUTED
        };
        let center = rect.right_center() - vec2(4.0, 0.0);
        painter.add(egui::Shape::line(
            vec![
                center + vec2(-1.5, -3.0),
                center + vec2(1.5, 0.0),
                center + vec2(-1.5, 3.0),
            ],
            Stroke::new(1.2, color),
        ));
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.detail = Some(service);
        }
    }

    /// Shown on the first launch when a service other than Codex is installed.
    fn choose_ui(&mut self, ui: &mut egui::Ui, choices: Vec<ServiceId>) {
        ui.add_space(17.0);
        ui.label(RichText::new("表示するサービスを選んでください").size(13.0));
        ui.add_space(6.0);
        let mut chosen = None;
        for &service in &choices {
            if ui.button(service.name()).clicked() {
                chosen = Some(vec![service]);
            }
        }
        if choices.len() > 1 && ui.button("まとめて表示").clicked() {
            chosen = Some(choices);
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new("あとから設定で変更できます。")
                .size(11.0)
                .color(MUTED),
        );
        if let Some(services) = chosen {
            self.choices = None;
            self.settings.services.clear();
            for service in services {
                self.select(ui.ctx(), service, true);
            }
            if self.settings.services.is_empty() {
                self.settings.services.push(ServiceId::Codex);
                self.services_changed(ui.ctx());
            }
        }
    }

    /// Codex alone needs no choice; connecting another tool waits for the user to pick it.
    fn first_run(&mut self) {
        self.settings.first_run = false;
        let found: Vec<_> = ServiceId::ALL
            .into_iter()
            .filter(|service| service.detected())
            .collect();
        if found.iter().any(|service| service.received()) {
            self.choices = Some(found);
        }
    }

    fn usage_ui(&mut self, ui: &mut egui::Ui, service: ServiceId) {
        let now = Local::now().timestamp();
        let source = self.source(service);
        let snapshot = self.snapshot(service);
        // Model families with their own limits, such as Gemini and Claude・GPT, get equal size.
        let (blocks, lines) = match snapshot {
            Some(usage) if card::has_families(usage) => (
                card::blocks(usage, &source, now),
                card::money_lines(usage, true),
            ),
            _ => (
                vec![card::hero(snapshot, &source, now)],
                snapshot.map_or_else(Vec::new, |usage| card::lines(usage, source.received, now)),
            ),
        };
        card::stack_ui(ui, &blocks, 17.0);
        // Only some plans earn reset credits; keep the row in place until the first response.
        if service == ServiceId::Codex
            && self
                .usage
                .as_ref()
                .is_none_or(|usage| usage.reset_credits.is_some())
        {
            ui.add_space(21.0);
            self.reset_credits_ui(ui, now);
        }
        for (index, line) in lines.iter().enumerate() {
            ui.add_space(if index == 0 { 18.0 } else { 12.0 });
            card::line_ui(ui, line);
        }
        if service == ServiceId::Codex
            && let Some(error) = &self.error
        {
            ui.add_space(15.0);
            ui.label(RichText::new(&error.message).size(11.0).color(MUTED));
            if matches!(error.kind, ErrorKind::Setup | ErrorKind::Login)
                && ui.small_button("設定を開く").clicked()
            {
                self.settings_open = true;
            }
        } else if service.received() && !self.settings.bridges.contains_key(&service) {
            ui.add_space(15.0);
            ui.label(
                RichText::new(format!("{}に連携していません", service.name()))
                    .size(11.0)
                    .color(MUTED),
            );
            if ui.small_button("設定を開く").clicked() {
                self.settings_open = true;
            }
        }

        // Keep the footer in the content flow so expanded details cannot overlap it.
        let used_height = ui.cursor().top() - ui.min_rect().top();
        ui.add_space((HEIGHT - 42.0 - used_height - 24.0).max(22.0));
        self.footer_ui(ui, &[service]);
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
                credit_expiry_label(credit.expires_at, now)
            } else if self.worker.is_some() && self.usage.is_none() {
                "取得中…".into()
            } else {
                "有効期限を確認中".into()
            };
            ui.label(RichText::new(label).size(11.0).color(MUTED));
        }
    }

    /// When the numbers on screen were last updated, with a refresh button for Codex.
    fn footer_ui(&mut self, ui: &mut egui::Ui, services: &[ServiceId]) {
        let codex = services.contains(&ServiceId::Codex);
        let latest = services
            .iter()
            .filter_map(|&service| self.snapshot(service))
            .map(|snapshot| snapshot.observed_at)
            .max();
        let status = if codex && self.worker.is_some() {
            "更新中…".into()
        } else if let Some(time) = latest {
            // A failed Codex fetch keeps showing the numbers it fetched before.
            let caption = if codex && services.len() == 1 && self.error.is_some() {
                "前回取得"
            } else {
                "更新"
            };
            format!("{caption} {}", time_label(time))
        } else if codex {
            "5分ごとに更新".into()
        } else {
            String::new()
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(status).size(10.0).color(MUTED));
            if !codex {
                return;
            }
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

    /// Choose the services to show and see whether each one is connected.
    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(17.0);
        ui.label(RichText::new("表示するサービス").strong());
        ui.add_space(4.0);
        // Selected services in display order, then the others.
        let listed: Vec<_> = self
            .settings
            .services
            .iter()
            .copied()
            .chain(
                ServiceId::ALL
                    .into_iter()
                    .filter(|service| !self.shows(*service)),
            )
            .collect();
        for service in listed {
            self.service_row(ui, service);
        }
        if self.shows(ServiceId::Codex)
            && (self.codex_missing() || self.settings.codex_path.is_some())
        {
            ui.add_space(8.0);
            self.codex_location_ui(ui);
        }
        if let Some(notice) = &self.notice {
            ui.add_space(8.0);
            ui.label(RichText::new(notice).size(11.0).color(MUTED));
        }
        if let Some(service) = self.failed_link
            && let Ok(command) = bridge::command(service)
            && ui.small_button("コマンドをコピー").clicked()
        {
            ui.ctx().copy_text(command);
        }
        ui.add_space(18.0);
        ui.label(
            RichText::new(format!("Codex Usage Widget  {}", env!("CARGO_PKG_VERSION")))
                .size(10.0)
                .color(MUTED),
        );
    }

    /// One service: whether it is shown, how it stands, and its place in the order.
    fn service_row(&mut self, ui: &mut egui::Ui, service: ServiceId) {
        let position = self.settings.services.iter().position(|s| *s == service);
        let shown = position.is_some();
        // Codex is looked up when it is fetched; tools that report on their own must be installed.
        let available = shown || !service.received() || service.detected();
        ui.horizontal(|ui| {
            let mut checked = shown;
            // At least one service stays selected.
            let locked = shown && self.settings.services.len() == 1;
            let label = if service.experimental() {
                format!("{}（実験的）", service.name())
            } else {
                service.name().into()
            };
            if ui
                .add_enabled(
                    available && !locked,
                    egui::Checkbox::new(&mut checked, label),
                )
                .changed()
            {
                self.select(ui.ctx(), service, checked);
            }
            if let Some(index) = position.filter(|index| *index > 0) {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if move_up_button(ui).clicked() {
                        self.settings.services.swap(index, index - 1);
                        self.services_changed(ui.ctx());
                    }
                });
            }
        });
        let status = self.service_status(service, available);
        if !status.is_empty() {
            ui.horizontal(|ui| {
                // Line up with the checkbox label.
                ui.add_space(18.0);
                ui.label(RichText::new(status).size(11.0).color(MUTED));
            });
        }
        ui.add_space(2.0);
    }

    fn service_status(&self, service: ServiceId, available: bool) -> String {
        if !available {
            return "見つかりません".into();
        }
        if !self.shows(service) {
            return String::new();
        }
        if service == ServiceId::Codex {
            return self.codex_status();
        }
        match (
            self.settings.bridges.contains_key(&service),
            self.received.get(&service),
        ) {
            (false, _) => "連携していません".into(),
            (true, Some(snapshot)) => format!("{}に受信", time_label(snapshot.observed_at)),
            (true, None) => format!("{}を使うと届きます", service.name()),
        }
    }

    fn codex_status(&self) -> String {
        if let Some(error) = &self.error {
            return match error.kind {
                ErrorKind::Setup => "見つかりません",
                ErrorKind::Login => "ログインが必要です",
                ErrorKind::Connection => "接続できません",
                ErrorKind::Unsupported => "Codexの更新が必要です",
                ErrorKind::Cancelled => "",
            }
            .into();
        }
        if self.worker.is_some() && self.usage.is_none() {
            return "確認中…".into();
        }
        match self
            .account
            .as_ref()
            .and_then(|account| account.plan.as_deref())
        {
            Some(plan) => format!("接続中（{}）", plan_name(plan)),
            None if self.usage.is_some() => "接続中".into(),
            None => String::new(),
        }
    }

    fn codex_missing(&self) -> bool {
        self.error
            .as_ref()
            .is_some_and(|error| matches!(error.kind, ErrorKind::Setup | ErrorKind::Login))
    }

    /// Only needed when Codex cannot be found automatically.
    fn codex_location_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(self.worker.is_none(), |ui| {
            ui.horizontal(|ui| {
                if ui.button("Codexの場所を選択").clicked() {
                    let dialog = rfd::FileDialog::new().set_title("公式Codexの実行ファイルを選択");
                    #[cfg(windows)]
                    let dialog = dialog.add_filter("Codex", &["exe", "cmd"]);
                    if let Some(path) = dialog.pick_file() {
                        self.settings.codex_path = Some(path);
                        self.reset_connection(ui.ctx());
                    }
                }
                if self.settings.codex_path.is_some() && ui.button("自動検出").clicked() {
                    self.settings.codex_path = None;
                    self.reset_connection(ui.ctx());
                }
            });
        });
        if self.codex_missing() {
            ui.hyperlink_to(
                "公式の導入・ログイン手順",
                "https://learn.chatgpt.com/docs/quickstart?setup=app",
            );
        }
    }

    /// Showing a service that reports on its own also connects it; hiding it undoes that.
    fn select(&mut self, ctx: &egui::Context, service: ServiceId, show: bool) {
        self.notice = None;
        self.failed_link = None;
        if service.received() {
            let result = if show {
                self.link(service)
            } else {
                self.unlink(service)
            };
            if let Err(message) = result {
                self.notice = Some(format!("{}：{message}", service.name()));
                self.failed_link = show.then_some(service);
                return;
            }
        }
        if show {
            self.settings.services.push(service);
        } else {
            self.settings
                .services
                .retain(|selected| *selected != service);
        }
        self.services_changed(ctx);
    }

    fn services_changed(&mut self, ctx: &egui::Context) {
        if self.settings.services.len() == 1
            || self.detail.is_some_and(|service| !self.shows(service))
        {
            self.detail = None;
        }
        self.persist();
        self.sync_watcher(ctx);
        self.update_tray();
        // Codex starts fetching when it is newly shown; otherwise its schedule continues.
        if self.next_refresh.is_none() && self.worker.is_none() {
            self.refresh(ctx, true);
        }
    }

    fn link(&mut self, service: ServiceId) -> Result<(), String> {
        if self.settings.bridges.contains_key(&service) {
            return Ok(());
        }
        let record = bridge::link(service, None)?;
        self.settings.bridges.insert(service, record);
        self.notice = Some(format!("{}に連携しました", service.name()));
        Ok(())
    }

    fn unlink(&mut self, service: ServiceId) -> Result<(), String> {
        let Some(record) = self.settings.bridges.get(&service).cloned() else {
            return Ok(());
        };
        let restored = bridge::unlink(service, &record)?;
        self.settings.bridges.remove(&service);
        self.received.remove(&service);
        // Numbers that can no longer be updated would only mislead.
        if let Some(path) = bridge::received_path(service) {
            let _ = fs::remove_file(path);
        }
        self.notice = Some(if restored {
            format!("{}の連携を解除しました", service.name())
        } else {
            format!(
                "{}の設定は変更済みのため、そのままにしました",
                service.name()
            )
        });
        Ok(())
    }

    /// Keep registrations pointing at this executable after the app moves or is renamed.
    fn relink(&mut self) {
        let mut changed = false;
        for (service, record) in self.settings.bridges.clone() {
            if let Some(updated) = bridge::relink(service, &record) {
                self.settings.bridges.insert(service, updated);
                changed = true;
            }
        }
        if changed {
            self.persist();
        }
    }
}

impl Widget {
    fn render(&mut self, ui: &mut egui::Ui) {
        let compact = self.settings.bar_mode && !self.settings_open;
        let size = if compact {
            vec2(self.bar_width(ui), BAR_HEIGHT)
        } else {
            window_size(false)
        };
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
                    } else if let Some(choices) = self.choices.clone() {
                        self.choose_ui(ui, choices);
                    } else if let Some(service) = self.shown() {
                        self.usage_ui(ui, service);
                    } else {
                        self.combined_ui(ui);
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
        if !self.started {
            self.started = true;
            self.relink();
            self.load_icons(ctx);
            if self.settings.first_run {
                self.first_run();
            }
        }
        self.sync_watcher(ctx);
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
        if let Some(watcher) = self.watcher.take() {
            watcher.stop();
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

fn move_up_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "上へ"));
    let color = if response.hovered() {
        FOREGROUND
    } else {
        MUTED
    };
    let center = rect.center();
    ui.painter().add(egui::Shape::line(
        vec![
            center + vec2(-4.0, 2.0),
            center + vec2(0.0, -2.0),
            center + vec2(4.0, 2.0),
        ],
        Stroke::new(1.3, color),
    ));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Codex reports plan ids such as "plus"; Pro tiers share one name.
fn plan_name(plan: &str) -> String {
    if plan.starts_with("pro") {
        return "Pro".into();
    }
    let mut letters = plan.chars();
    letters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(letters).collect()
    })
}

/// A time of day, with the date when it is not today.
fn time_label(time: DateTime<Local>) -> String {
    let format = if time.date_naive() == Local::now().date_naive() {
        "%H:%M"
    } else {
        "%-m/%-d %H:%M"
    };
    time.format(format).to_string()
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
    fn countdown_and_credit_cards_fit_inside_the_card() {
        use crate::quota::{Blocked, Cap, Group, Span, Window};
        let ctx = egui::Context::default();
        let mut widget = std::mem::ManuallyDrop::new(Widget::new(&ctx, Settings::default()));
        let now = Local::now().timestamp();
        let blocked = Snapshot {
            groups: vec![Group::new(
                None,
                vec![
                    Window {
                        span: Span::Minutes(300),
                        remaining: Some(0.0),
                        resets_at: Some(now + 23 * 3600 + 59 * 60),
                    },
                    Window {
                        span: Span::Minutes(10080),
                        remaining: Some(54.0),
                        resets_at: Some(now + 86_400),
                    },
                ],
            )],
            cap: None,
            balance: None,
            reset_credits: None,
            blocked: Some(Blocked {
                label: "5時間の枠".into(),
                until: Some(now + 23 * 3600 + 59 * 60),
                minutes: Some(300),
            }),
            observed_at: Local::now(),
        };
        let credit = Snapshot {
            groups: vec![Group::default()],
            cap: Some(Cap {
                label: "月間クレジット上限".into(),
                remaining: Some(62.0),
                detail: Some("314 / 500 クレジット使用".into()),
                resets_at: Some(now + 86_400),
            }),
            balance: Some("1250".into()),
            reset_credits: None,
            blocked: None,
            observed_at: Local::now(),
        };
        for (usage, expected) in [
            (blocked, "あと 23時間59分"),
            (credit, "月間クレジット上限の残り"),
        ] {
            widget.usage = Some(usage);
            widget.last_started = Some(Instant::now());
            let mut found = false;
            for _ in 0..3 {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, widget.size)),
                        focused: true,
                        ..Default::default()
                    },
                    |ui| widget.render(ui),
                );
                let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, widget.size);
                for shape in &output.shapes {
                    if let egui::Shape::Text(text) = &shape.shape {
                        assert!(
                            bounds.contains_rect(shape.shape.visual_bounding_rect()),
                            "{}",
                            text.galley.job.text
                        );
                        found |= text.galley.job.text == expected;
                    }
                }
                output.drop_without_applying_deltas();
            }
            assert!(found, "{expected}");
            assert_eq!(widget.size.x, WIDTH);
        }
        assert!(widget.worker.is_none());
    }

    #[test]
    fn combined_view_lists_each_service_and_opens_its_card() {
        use crate::quota::{Blocked, Group, Span, Window};
        let ctx = egui::Context::default();
        let mut widget = std::mem::ManuallyDrop::new(Widget::new(
            &ctx,
            Settings {
                services: vec![ServiceId::Codex, ServiceId::ClaudeCode],
                ..Default::default()
            },
        ));
        let now = Local::now().timestamp();
        let window = |minutes, remaining, resets_at| Window {
            span: Span::Minutes(minutes),
            remaining: Some(remaining),
            resets_at: Some(resets_at),
        };
        widget.usage = Some(Snapshot {
            groups: vec![Group::new(
                None,
                vec![
                    window(10080, 62.0, now + 86_400),
                    window(300, 88.0, now + 3600),
                ],
            )],
            cap: None,
            balance: None,
            reset_credits: None,
            blocked: None,
            observed_at: Local::now(),
        });
        let until = now + 2 * 3600 + 13 * 60;
        widget.received.insert(
            ServiceId::ClaudeCode,
            Snapshot {
                groups: vec![Group::new(
                    None,
                    vec![window(300, 0.0, until), window(10080, 54.0, now + 86_400)],
                )],
                cap: None,
                balance: None,
                reset_credits: None,
                blocked: Some(Blocked {
                    label: "5時間の枠".into(),
                    until: Some(until),
                    minutes: Some(300),
                }),
                observed_at: Local::now(),
            },
        );
        widget.last_started = Some(Instant::now());
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
        let texts = |widget: &mut Widget| {
            let output = step(widget, vec![]);
            let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, widget.size);
            let mut texts = Vec::new();
            for shape in &output.shapes {
                if let egui::Shape::Text(text) = &shape.shape {
                    let rect = shape.shape.visual_bounding_rect();
                    assert!(bounds.contains_rect(rect), "{}", text.galley.job.text);
                    texts.push((text.galley.job.text.clone(), rect));
                }
            }
            output.drop_without_applying_deltas();
            texts
        };
        let click = |widget: &mut Widget, pos: egui::Pos2| {
            for pressed in [true, false] {
                step(
                    widget,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                )
                .drop_without_applying_deltas();
            }
        };
        texts(&mut widget);
        let combined = texts(&mut widget);
        let has = |texts: &[(String, egui::Rect)], wanted: &str| {
            texts.iter().any(|(text, _)| text == wanted)
        };
        for wanted in [
            "利用状況",
            "Codex",
            "Claude Code",
            "5時間の残り",
            "週次の残り",
            "62%",
            "リキャスト中（5時間の枠）",
        ] {
            assert!(has(&combined, wanted), "{wanted}");
        }
        // Each window is laid out as on the single card, so the reset caption repeats.
        let resets = combined
            .iter()
            .filter(|(text, _)| text == "リセット")
            .count();
        assert_eq!(resets, 3);
        assert_eq!(widget.size.x, WIDTH);
        let section = combined
            .iter()
            .find(|(text, _)| text == "Claude Code")
            .unwrap()
            .1;
        click(&mut widget, section.center());
        assert_eq!(widget.detail, Some(ServiceId::ClaudeCode));
        texts(&mut widget);
        let detail = texts(&mut widget);
        assert!(has(&detail, "リキャスト中（5時間の枠）"));
        assert!(has(&detail, "週次の残り"));
        click(&mut widget, egui::pos2(28.0, 33.0));
        assert_eq!(widget.detail, None);
        widget.settings.bar_mode = true;
        texts(&mut widget);
        let bar = texts(&mut widget);
        let line: Vec<_> = bar.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(line, ["Codex 62%", "Claude あと2:13", "⋯"]);
        // Logos the user provides take the place of the names.
        for service in [ServiceId::Codex, ServiceId::ClaudeCode] {
            let image = egui::ColorImage::filled([2, 2], Color32::WHITE);
            let texture = ctx.load_texture(service.key(), image, egui::TextureOptions::LINEAR);
            widget.icons.insert(service, texture);
        }
        texts(&mut widget);
        let bar = texts(&mut widget);
        let line: Vec<_> = bar.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(line, ["62%", "あと2:13", "⋯"]);
        assert!(widget.size.x > BAR_WIDTH);
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
            widget.usage = Some(Snapshot {
                groups: vec![crate::quota::Group::new(
                    None,
                    vec![crate::quota::Window {
                        span: crate::quota::Span::Minutes(10080),
                        remaining,
                        resets_at: None,
                    }],
                )],
                cap: None,
                balance: None,
                reset_credits: None,
                blocked: None,
                observed_at: Local::now(),
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
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, widget.size);
        let mut texts = Vec::new();
        for shape in &output.shapes {
            if let egui::Shape::Text(text) = &shape.shape {
                assert!(bounds.contains_rect(shape.shape.visual_bounding_rect()));
                texts.push(text.galley.job.text.clone());
            }
        }
        // The settings list every service; the menu stays closed.
        for wanted in [
            "設定",
            "表示するサービス",
            "Codex",
            "Claude Code",
            "Antigravity CLI（実験的）",
        ] {
            assert!(texts.iter().any(|text| text == wanted), "{wanted}");
        }
        for menu in ["最前面に表示", "ログイン時に自動起動", "終了"] {
            assert!(!texts.iter().any(|text| text == menu), "{menu}");
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
