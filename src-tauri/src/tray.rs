use std::sync::Mutex;

use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, Runtime, WebviewWindow,
};
use tracing::{info, warn};

use crate::config::AppConfig;

/// Tray icon PNG embedded at compile time so no path resolution is needed.
const TRAY_ICON_BYTES: &[u8] = include_bytes!("../icons/32x32.png");

/// Build the system-tray icon and attach its menu event handler.
pub fn setup<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let status_text = app
        .state::<Mutex<AppConfig>>()
        .lock()
        .map(|g| format!(
            "{} server(s) · {} project(s) configured",
            g.servers.len(),
            g.projects.len()
        ))
        .unwrap_or_else(|_| "Config unavailable".to_owned());

    let open_item = MenuItemBuilder::with_id("open_config", "Open Configuration").build(app)?;
    let copy_item = MenuItemBuilder::with_id("copy_config", "Copy Claude Desktop Config").build(app)?;
    let status_item = MenuItemBuilder::with_id("status", status_text)
        .enabled(false)
        .build(app)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "Quit Remote Dev Bridge").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&open_item)
        .item(&copy_item)
        .item(&sep1)
        .item(&status_item)
        .item(&sep2)
        .item(&sep3)
        .item(&quit_item)
        .build()?;

    let icon = Image::from_bytes(TRAY_ICON_BYTES)?;

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .icon_as_template(true) // macOS: monochrome template adapts to dark/light menu bar
        .tooltip("Remote Dev Bridge — MCP SSH Server")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().0.as_str() {
            "open_config" => show_config_window(app),
            "copy_config" => copy_claude_config(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    Ok(())
}

/// Copy the Claude Desktop MCP config snippet to the system clipboard.
fn copy_claude_config<R: Runtime>(app: &AppHandle<R>) {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "remote-dev-bridge".to_owned());

    let snippet = serde_json::json!({
        "mcpServers": {
            "remote-dev-bridge": {
                "command": exe,
                "args": ["--mcp-stdio"]
            }
        }
    });

    let text = serde_json::to_string_pretty(&snippet)
        .unwrap_or_else(|_| String::new());

    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&text)) {
        Ok(_) => {
            info!("Claude Desktop config copied to clipboard");
            // Briefly update tooltip as visual confirmation.
            if let Some(tray) = app.tray_by_id("main") {
                let _ = tray.set_tooltip(Some("✓ Copied to clipboard!"));
                let app = app.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    if let Some(t) = app.tray_by_id("main") {
                        let _ = t.set_tooltip(Some("Remote Dev Bridge — MCP SSH Server"));
                    }
                });
            }
        }
        Err(e) => warn!("Failed to copy to clipboard: {e}"),
    }
}

/// Show the configuration window, creating it if it was closed, or focusing
/// it if already open.
fn show_config_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    // If the window doesn't exist (e.g. was explicitly closed and we didn't
    // prevent it), `get_webview_window` returns None. The window starts
    // hidden via tauri.conf.json so this branch is only hit on edge cases.
}

/// Wire the config window to hide (not close) when the user clicks its close
/// button, keeping it available for the next tray-menu invocation.
pub fn attach_close_handler(window: &WebviewWindow) {
    let win = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = win.hide();
        }
    });
}
