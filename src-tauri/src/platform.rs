//! Platform-specific Dock / activation behaviour.
//!
//! The app is a tray application that also owns one real window. On macOS we
//! want both: a Dock icon while the configuration window is open (so clicking
//! the icon behaves like a normal app), and no Dock icon once it is closed
//! (so only the menu bar item remains while the MCP bridge keeps running).
//!
//! A static `LSUIElement = true` in Info.plist cannot express that - it makes
//! the process a permanent UI-element app, and clicking the Dock icon becomes a
//! no-op. So the plist stays silent and we transform the process at runtime.

/// Show or hide the Dock icon at runtime.
///
/// MUST be called on the main thread - Cocoa guarantees nothing otherwise.
/// Use `AppHandle::run_on_main_thread` from anywhere else.
#[cfg(target_os = "macos")]
pub fn set_dock_icon_visible(visible: bool) {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn TransformProcessType(psn: *const [u32; 2], r#type: u32) -> i32;
    }
    const K_CURRENT_PROCESS: [u32; 2] = [0, 2]; // kCurrentProcess
    const TO_FOREGROUND: u32 = 1; // kProcessTransformToForegroundApplication
    const TO_UI_ELEMENT: u32 = 4; // kProcessTransformToUIElementApplication

    let transform = if visible { TO_FOREGROUND } else { TO_UI_ELEMENT };
    unsafe {
        TransformProcessType(&K_CURRENT_PROCESS, transform);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon_visible(_visible: bool) {}
