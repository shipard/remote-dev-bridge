// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod error;
mod mcp;
mod ssh;
mod tray;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use clap::Parser;
use tracing::info;
use tracing_appender::non_blocking::WorkerGuard;

use commands::IsFirstRun;

#[derive(Parser, Debug)]
#[command(
    name = "remote-dev-bridge",
    version,
    about = "Remote Dev Bridge — MCP SSH filesystem server"
)]
struct Args {
    /// Run as MCP server on stdin/stdout instead of launching the desktop UI.
    #[arg(long)]
    mcp_stdio: bool,
}

fn main() {
    let args = Args::parse();
    let _guard = init_logging(/* desktop */ !args.mcp_stdio);

    if args.mcp_stdio {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(run_mcp_stdio());
    } else {
        run_desktop_app();
    }
}

// ── Logging ───────────────────────────────────────────────────────────────────

/// Initialise tracing.  Returns a `WorkerGuard` that must be kept alive for
/// the lifetime of the process (dropping it flushes the async writer).
fn init_logging(desktop_mode: bool) -> Option<WorkerGuard> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    if desktop_mode {
        let log_dir = platform_log_dir();
        let _ = std::fs::create_dir_all(&log_dir);
        cleanup_old_logs(&log_dir, 5);

        let file_appender = tracing_appender::rolling::daily(&log_dir, "app.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        tracing_subscriber::fmt()
            .with_writer(non_blocking)
            .with_env_filter(filter)
            .with_ansi(false)
            .init();

        Some(guard)
    } else {
        // MCP mode: structured JSON to stderr (stdout is owned by the transport).
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .json()
            .init();
        None
    }
}

fn platform_log_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir()
            .unwrap_or_default()
            .join("Library/Logs/remote-dev-bridge")
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_dir()
            .unwrap_or_default()
            .join("remote-dev-bridge")
            .join("logs")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::data_local_dir()
            .unwrap_or_default()
            .join("remote-dev-bridge")
            .join("logs")
    }
}

/// Delete log files beyond the `keep` most-recently-modified ones.
fn cleanup_old_logs(log_dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(log_dir) else { return };

    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("app.log")
        })
        .filter_map(|e| {
            let path = e.path();
            let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((mtime, path))
        })
        .collect();

    files.sort_by(|a, b| b.0.cmp(&a.0)); // newest first

    for (_, path) in files.iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

// ── MCP stdio mode ─────────────────────────────────────────────────────────────

async fn run_mcp_stdio() {
    info!("Starting in MCP stdio mode");

    let cfg = match config::load_config() {
        Ok(c) => {
            info!(
                servers = c.servers.len(),
                projects = c.projects.len(),
                "config loaded"
            );
            c
        }
        Err(e) => {
            eprintln!("fatal: cannot load config: {e}");
            std::process::exit(1);
        }
    };

    let sessions = ssh::SshSessionManager::new_shared(cfg.settings.clone());
    let server = mcp::McpServer::new(cfg, sessions);
    server.run().await;
}

// ── Desktop app mode ───────────────────────────────────────────────────────────

fn run_desktop_app() {
    info!("Starting in desktop app mode");

    // Detect first run *before* load_config creates the file.
    let first_run = !config::config_exists();

    let cfg = match config::load_config() {
        Ok(c) => {
            info!(
                servers = c.servers.len(),
                projects = c.projects.len(),
                "config loaded"
            );
            c
        }
        Err(e) => {
            tracing::warn!("Could not load config: {e} — using defaults");
            config::AppConfig::default()
        }
    };

    let sessions = ssh::SshSessionManager::new_shared(cfg.settings.clone());

    tauri::Builder::default()
        .manage(Mutex::new(cfg))
        .manage(sessions)
        .manage(IsFirstRun(first_run))
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config_cmd,
            commands::test_ssh_connection,
            commands::get_connection_status,
            commands::is_first_run,
            commands::get_binary_path,
        ])
        .setup(move |app| {
            use tauri::Manager;
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            tray::setup(app.handle())?;
            if let Some(window) = app.get_webview_window("main") {
                tray::attach_close_handler(&window);
                if first_run {
                    // Auto-open the config window on first launch.
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error running Tauri app");
}
