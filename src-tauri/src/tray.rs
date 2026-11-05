//! The system tray icon: shows whether a recording is running and offers start/stop, so a
//! recording can be controlled (and survives) without the main window.

use tan_domain::UiLanguage;
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::commands::recording;

const TRAY_ID: &str = "tan-tray";
const MENU_TOGGLE: &str = "toggle-recording";
const MENU_SHOW: &str = "show-window";
const MENU_QUIT: &str = "quit";
/// Label of the main window (`tauri.conf.json`).
pub const MAIN_WINDOW: &str = "main";

/// The tray's texts in one language.
struct TrayText {
    start: &'static str,
    stop: &'static str,
    show: &'static str,
    quit: &'static str,
    idle_tooltip: &'static str,
    recording_tooltip: &'static str,
}

fn text_for(language: UiLanguage) -> TrayText {
    match language {
        UiLanguage::Tr => TrayText {
            start: "Kaydı başlat",
            stop: "Kaydı durdur",
            show: "Pencereyi göster",
            quit: "Çık",
            idle_tooltip: "Tan",
            recording_tooltip: "Tan: kayıt sürüyor",
        },
        UiLanguage::En => TrayText {
            start: "Start recording",
            stop: "Stop recording",
            show: "Show window",
            quit: "Quit",
            idle_tooltip: "Tan",
            recording_tooltip: "Tan: recording",
        },
    }
}

/// What the tray needs to flip between its two states. Managed by Tauri.
pub struct TrayHandles {
    tray: TrayIcon<Wry>,
    toggle: MenuItem<Wry>,
    text: TrayText,
    idle_icon: Image<'static>,
    recording_icon: Image<'static>,
}

/// Builds the tray icon and its menu. The texts follow the UI language at startup.
pub fn init(app: &AppHandle, language: UiLanguage) -> tauri::Result<()> {
    let text = text_for(language);
    let toggle = MenuItem::with_id(app, MENU_TOGGLE, text.start, true, None::<&str>)?;
    let show = MenuItem::with_id(app, MENU_SHOW, text.show, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, text.quit, true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&toggle, &show, &separator, &quit])?;

    let idle_icon = base_icon(app);
    let recording_icon = with_recording_badge(&idle_icon);

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(idle_icon.clone())
        .tooltip(text.idle_tooltip)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu_event(app, &event))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    app.manage(TrayHandles {
        tray,
        toggle,
        text,
        idle_icon,
        recording_icon,
    });
    Ok(())
}

/// Switches the tray between "idle" and "recording" (icon badge, tooltip, menu label).
/// A missing tray (failed to build) is not an error: recording does not depend on it.
pub fn set_recording(app: &AppHandle, recording: bool) {
    let Some(handles) = app.try_state::<TrayHandles>() else {
        return;
    };
    let (icon, tooltip, label) = if recording {
        (
            &handles.recording_icon,
            handles.text.recording_tooltip,
            handles.text.stop,
        )
    } else {
        (
            &handles.idle_icon,
            handles.text.idle_tooltip,
            handles.text.start,
        )
    };
    if let Err(error) = handles.tray.set_icon(Some(icon.clone())) {
        tracing::debug!(%error, "cannot update the tray icon");
    }
    if let Err(error) = handles.tray.set_tooltip(Some(tooltip)) {
        tracing::debug!(%error, "cannot update the tray tooltip");
    }
    if let Err(error) = handles.toggle.set_text(label) {
        tracing::debug!(%error, "cannot update the tray menu");
    }
}

/// Brings the main window back (it is only hidden, never closed, while recording).
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn on_menu_event(app: &AppHandle, event: &MenuEvent) {
    match event.id().as_ref() {
        MENU_TOGGLE => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let result = if recording::is_recording(&app) {
                    recording::stop_running(&app).await
                } else {
                    recording::start_with_defaults(&app).await
                };
                if let Err(error) = result {
                    tracing::error!(code = ?error.code, "{}", error.message);
                }
            });
        }
        MENU_SHOW => show_main_window(app),
        MENU_QUIT => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                // Never throw a running recording away: settle it, then quit (the exit
                // handler holds the exit as well, this just avoids relying on it).
                recording::stop_and_settle(&app).await;
                app.exit(0);
            });
        }
        _ => {}
    }
}

/// The application icon (the default window icon), or a flat grey square if there is none.
fn base_icon(app: &AppHandle) -> Image<'static> {
    match app.default_window_icon() {
        Some(icon) => Image::new_owned(icon.rgba().to_vec(), icon.width(), icon.height()),
        None => Image::new_owned(vec![128; 32 * 32 * 4], 32, 32),
    }
}

/// A copy of `icon` with a red dot in the bottom-right corner.
fn with_recording_badge(icon: &Image<'static>) -> Image<'static> {
    let (width, height) = (icon.width(), icon.height());
    let mut rgba = icon.rgba().to_vec();
    let radius = f64::from(width.min(height)) * 0.3;
    let (cx, cy) = (
        f64::from(width) - radius - 1.0,
        f64::from(height) - radius - 1.0,
    );
    for y in 0..height {
        for x in 0..width {
            let (dx, dy) = (f64::from(x) + 0.5 - cx, f64::from(y) + 0.5 - cy);
            let distance = dx.hypot(dy);
            let i = ((y * width + x) * 4) as usize;
            if distance <= radius - 1.0 {
                rgba[i..i + 4].copy_from_slice(&[220, 38, 38, 255]);
            } else if distance <= radius {
                // One pixel white ring so the dot stands out on dark and light trays.
                rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    Image::new_owned(rgba, width, height)
}
