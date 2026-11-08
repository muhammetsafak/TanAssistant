//! The system tray icon: shows whether a recording is running or paused and offers
//! start/stop and pause/resume, so a recording can be controlled (and survives) without the
//! main window.

use tan_domain::UiLanguage;
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::commands::recording;

const TRAY_ID: &str = "tan-tray";
const MENU_TOGGLE: &str = "toggle-recording";
const MENU_PAUSE: &str = "pause-recording";
const MENU_SHOW: &str = "show-window";
const MENU_QUIT: &str = "quit";
/// Label of the main window (`tauri.conf.json`).
pub const MAIN_WINDOW: &str = "main";

// The tray PNGs are generated from `assets/brand/` by `scripts/gen-icons.sh` (44 px; the OS
// scales them down) and decoded at compile time by `include_image!`, so a broken file fails
// the build instead of the tray. macOS recolours template images to match the menu bar, so
// idle and paused use the black silhouettes there; Windows and Linux do not, so they get
// coloured ones. The recording silhouette is amber everywhere and is never a template.
#[cfg(target_os = "macos")]
fn idle_image() -> Image<'static> {
    tauri::include_image!("icons/tray/idle-template@2x.png")
}
#[cfg(not(target_os = "macos"))]
fn idle_image() -> Image<'static> {
    tauri::include_image!("icons/tray/idle-color@2x.png")
}
fn recording_image() -> Image<'static> {
    tauri::include_image!("icons/tray/recording@2x.png")
}
#[cfg(target_os = "macos")]
fn paused_image() -> Image<'static> {
    tauri::include_image!("icons/tray/paused-template@2x.png")
}
#[cfg(not(target_os = "macos"))]
fn paused_image() -> Image<'static> {
    tauri::include_image!("icons/tray/paused-color@2x.png")
}

/// The tray's texts in one language.
struct TrayText {
    start: &'static str,
    stop: &'static str,
    pause: &'static str,
    resume: &'static str,
    show: &'static str,
    quit: &'static str,
    idle_tooltip: &'static str,
    recording_tooltip: &'static str,
    paused_tooltip: &'static str,
}

/// What the tray shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    /// No recording.
    Idle,
    /// A recording is running.
    Recording,
    /// A recording is paused.
    Paused,
}

fn text_for(language: UiLanguage) -> TrayText {
    match language {
        UiLanguage::Tr => TrayText {
            start: "Kaydı başlat",
            stop: "Kaydı durdur",
            pause: "Duraklat",
            resume: "Devam et",
            show: "Pencereyi göster",
            quit: "Çık",
            idle_tooltip: "Tan",
            recording_tooltip: "Tan: kayıt sürüyor",
            paused_tooltip: "Tan: kayıt duraklatıldı",
        },
        UiLanguage::En => TrayText {
            start: "Start recording",
            stop: "Stop recording",
            pause: "Pause",
            resume: "Resume",
            show: "Show window",
            quit: "Quit",
            idle_tooltip: "Tan",
            recording_tooltip: "Tan: recording",
            paused_tooltip: "Tan: recording paused",
        },
    }
}

/// One tray image and whether macOS should treat it as a template (recolour it to the menu
/// bar). `template` is ignored on Windows and Linux.
struct TrayImage {
    image: Image<'static>,
    template: bool,
}

/// What the tray needs to flip between its states. Managed by Tauri.
pub struct TrayHandles {
    tray: TrayIcon<Wry>,
    toggle: MenuItem<Wry>,
    pause: MenuItem<Wry>,
    text: TrayText,
    idle_icon: TrayImage,
    recording_icon: TrayImage,
    paused_icon: TrayImage,
}

/// Builds the tray icon and its menu. The texts follow the UI language at startup.
pub fn init(app: &AppHandle, language: UiLanguage) -> tauri::Result<()> {
    let text = text_for(language);
    let toggle = MenuItem::with_id(app, MENU_TOGGLE, text.start, true, None::<&str>)?;
    // Only meaningful while a recording exists.
    let pause = MenuItem::with_id(app, MENU_PAUSE, text.pause, false, None::<&str>)?;
    let show = MenuItem::with_id(app, MENU_SHOW, text.show, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, text.quit, true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&toggle, &pause, &show, &separator, &quit])?;

    let idle_icon = TrayImage {
        image: idle_image(),
        template: true,
    };
    let recording_icon = TrayImage {
        image: recording_image(),
        template: false,
    };
    let paused_icon = TrayImage {
        image: paused_image(),
        template: true,
    };

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(idle_icon.image.clone())
        .icon_as_template(idle_icon.template)
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
        pause,
        text,
        idle_icon,
        recording_icon,
        paused_icon,
    });
    Ok(())
}

/// Switches the tray between "idle", "recording" and "paused" (icon, tooltip, menu
/// labels). A missing tray (failed to build) is not an error: recording does not depend on it.
pub fn set_state(app: &AppHandle, state: TrayState) {
    let Some(handles) = app.try_state::<TrayHandles>() else {
        return;
    };
    let text = &handles.text;
    let (icon, tooltip, label, pause_label) = match state {
        TrayState::Idle => (
            &handles.idle_icon,
            text.idle_tooltip,
            text.start,
            text.pause,
        ),
        TrayState::Recording => (
            &handles.recording_icon,
            text.recording_tooltip,
            text.stop,
            text.pause,
        ),
        TrayState::Paused => (
            &handles.paused_icon,
            text.paused_tooltip,
            text.stop,
            text.resume,
        ),
    };
    if let Err(error) = handles.pause.set_text(pause_label) {
        tracing::debug!(%error, "cannot update the tray pause label");
    }
    if let Err(error) = handles.pause.set_enabled(state != TrayState::Idle) {
        tracing::debug!(%error, "cannot update the tray pause item");
    }
    // Sets the image and its template flag together, so macOS does not flicker in between.
    if let Err(error) = handles
        .tray
        .set_icon_with_as_template(Some(icon.image.clone()), icon.template)
    {
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
                // A paused recording is stopped like a running one.
                let result = if recording::is_recording(&app) || recording::is_paused(&app) {
                    recording::stop_running(&app).await
                } else {
                    recording::start_with_defaults(&app).await
                };
                if let Err(error) = result {
                    tracing::error!(code = ?error.code, "{}", error.message);
                }
            });
        }
        MENU_PAUSE => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let result = if recording::is_paused(&app) {
                    recording::resume_current(&app).await
                } else {
                    recording::pause_current(&app).await
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
