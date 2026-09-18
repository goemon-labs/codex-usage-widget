use auto_launch::{AutoLaunch, AutoLaunchBuilder};
use eframe::egui;
use std::sync::mpsc::Sender;
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

#[derive(Clone, Copy)]
pub enum Action {
    Show,
    Refresh,
    Settings,
    Quit,
}

pub fn create_tray(
    tx: Sender<Action>,
    ctx: &egui::Context,
) -> Result<TrayIcon, Box<dyn std::error::Error>> {
    let context = ctx.clone();
    let menu_tx = tx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = match event.id.as_ref() {
            "show" => Action::Show,
            "refresh" => Action::Refresh,
            "settings" => Action::Settings,
            "quit" => Action::Quit,
            _ => return,
        };
        let _ = menu_tx.send(action);
        context.request_repaint();
    }));
    let menu = Menu::new();
    menu.append_items(&[
        &MenuItem::with_id("show", "ウィジェットを表示", true, None),
        &MenuItem::with_id("refresh", "今すぐ更新", true, None),
        &MenuItem::with_id("settings", "設定", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id("quit", "終了", true, None),
    ])?;
    let context = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        if matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        ) {
            let _ = tx.send(Action::Show);
            context.request_repaint();
        }
    }));
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/tray.png"))?;
    Ok(TrayIconBuilder::new()
        .with_id("codex-usage-widget")
        .with_menu(Box::new(menu))
        .with_icon(Icon::from_rgba(icon.rgba, icon.width, icon.height)?)
        .with_icon_as_template(cfg!(target_os = "macos"))
        .with_menu_on_left_click(cfg!(target_os = "macos"))
        .with_tooltip("Codex Usage Widget")
        .build()?)
}

pub fn monitor_rect(window: &winit::window::Window) -> Option<egui::Rect> {
    let monitor = window.current_monitor()?;
    let scale = window.scale_factor();
    let origin = monitor.position().to_logical::<f32>(scale);
    let size = monitor.size().to_logical::<f32>(scale);
    Some(egui::Rect::from_min_size(
        egui::pos2(origin.x, origin.y),
        egui::vec2(size.width, size.height),
    ))
}

pub fn configure_window(cc: &eframe::CreationContext<'_>) {
    #[cfg(windows)]
    if let Some(window) = cc.winit_window() {
        use winit::platform::windows::{CornerPreference, WindowExtWindows};
        // The app paints one rounded outline; avoid a second border or corner mask from DWM.
        window.set_corner_preference(CornerPreference::DoNotRound);
        window.set_border_color(None);
        window.set_undecorated_shadow(false);
    }
    #[cfg(not(windows))]
    let _ = cc;
}

pub fn auto_launch() -> Result<AutoLaunch, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let app_path = executable.to_string_lossy();
    #[cfg(windows)]
    // The Windows startup entry is a command line, so installation paths may contain spaces.
    let app_path = format!("\"{app_path}\"");
    AutoLaunchBuilder::new()
        .set_app_name("Codex Usage Widget")
        .set_app_path(&app_path)
        .set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser)
        .set_macos_launch_mode(auto_launch::MacOSLaunchMode::LaunchAgent)
        .build()
        .map_err(|error| error.to_string())
}

pub fn keep_window_visible(cc: &eframe::CreationContext<'_>) {
    let Some(window) = cc.winit_window() else {
        return;
    };
    let Ok(position) = window.outer_position() else {
        return;
    };
    let size = window.outer_size();
    let intersects_monitor = window.available_monitors().any(|monitor| {
        let origin = monitor.position();
        let screen = monitor.size();
        position.x + size.width as i32 > origin.x + 32
            && position.y + size.height as i32 > origin.y + 32
            && position.x < origin.x + screen.width as i32 - 32
            && position.y < origin.y + screen.height as i32 - 32
    });
    if !intersects_monitor && let Some(monitor) = window.primary_monitor() {
        let origin = monitor.position().to_logical::<f32>(window.scale_factor());
        cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                origin.x + 24.0,
                origin.y + 24.0,
            )));
    }
}
