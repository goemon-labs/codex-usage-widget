#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod codex;
mod codex_app;
mod instance;
mod platform;
mod quota;
mod settings;

use eframe::egui;
use std::sync::{Arc, atomic::AtomicBool};

fn main() -> eframe::Result {
    let settings = settings::Settings::load();
    let check_app = std::env::args().any(|argument| argument == "--check-app");
    if check_app || std::env::args().any(|argument| argument == "--check") {
        #[cfg(windows)]
        // The GUI executable can also report diagnostics to an existing terminal.
        unsafe {
            use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
        let installation = if check_app {
            codex::find_app()
        } else {
            codex::find_codex(settings.codex_path.as_deref())
        };
        let result = installation
            .ok_or_else(|| codex::SETUP_GUIDANCE.to_string())
            .and_then(|installation| {
                codex::fetch(&installation, &Arc::new(AtomicBool::new(false)), |_| {})
                    .map_err(|error| error.message)
            });
        match result {
            Ok(usage) => {
                let window = |window: Option<quota::Window>| {
                    window.map(|w| serde_json::json!({
                    "window_minutes": w.minutes, "remaining_percent": w.remaining, "resets_at": w.resets_at,
                }))
                };
                println!(
                    "{}",
                    serde_json::json!({
                        "weekly": window(usage.weekly), "short": window(usage.short),
                        "reset_credits": usage.reset_credits.map(|resets| serde_json::json!({
                            "available_count": resets.available_count,
                            "expires_at": resets.credits.map(|credits| credits.into_iter().map(|credit| credit.expires_at).collect::<Vec<_>>()),
                        })),
                    })
                );
                return Ok(());
            }
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(1);
            }
        }
    }
    let Some(instance) = instance::Instance::acquire()
        .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?
    else {
        return Ok(());
    };
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("bundled icon is valid");
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Codex Usage Widget")
        .with_inner_size(app::window_size(settings.bar_mode))
        .with_decorations(false)
        .with_has_shadow(false)
        .with_resizable(false)
        .with_transparent(true)
        .with_taskbar(false)
        .with_icon(icon)
        .with_window_level(if settings.always_on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        });
    if let Some([x, y]) = settings
        .position
        .filter(|p| p.iter().all(|v| v.is_finite()))
    {
        viewport = viewport.with_position(egui::pos2(x, y));
    }
    let mut options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        centered: settings.position.is_none(),
        persist_window: false,
        #[cfg(target_os = "macos")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder.with_activation_policy(ActivationPolicy::Accessory);
        })),
        ..Default::default()
    };
    // Waiting for the monitor refresh blocks window dragging on some Windows drivers.
    options.glow_options.vsync = false;
    eframe::run_native(
        "Codex Usage Widget",
        options,
        Box::new(move |cc| {
            platform::configure_window(cc);
            platform::keep_window_visible(cc);
            let mut widget = app::Widget::new(&cc.egui_ctx, settings);
            widget.attach_instance(&cc.egui_ctx, instance)?;
            Ok(Box::new(widget))
        }),
    )
}
