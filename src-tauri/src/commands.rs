use std::collections::HashMap;
use std::sync::Mutex;

use tauri::State;

use crate::config::{save_config, validate_config, AppConfig};
use crate::ssh::{test_connection, SharedSessionManager};

/// Newtype so `State<IsFirstRun>` doesn't clash with any other `State<bool>`.
pub struct IsFirstRun(pub bool);

/// Return the current in-memory config to the frontend.
#[tauri::command]
pub fn get_config(state: State<Mutex<AppConfig>>) -> Result<AppConfig, String> {
    state.lock().map(|g| g.clone()).map_err(|e| e.to_string())
}

/// Validate, persist to disk, and update in-memory config.
#[tauri::command]
pub fn save_config_cmd(
    config: AppConfig,
    state: State<Mutex<AppConfig>>,
) -> Result<(), String> {
    validate_config(&config).map_err(|e| e.to_string())?;
    save_config(&config).map_err(|e| e.to_string())?;
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    *guard = config;
    Ok(())
}

/// Connect to a server and return a short info string (hostname, OS).
/// Used by the "Test Connection" button.
#[tauri::command]
pub async fn test_ssh_connection(
    server_id: String,
    state: State<'_, Mutex<AppConfig>>,
) -> Result<String, String> {
    // Grab the data under the lock, then release before awaiting.
    let (server_cfg, settings) = {
        let cfg = state.lock().map_err(|e| e.to_string())?;
        let s = cfg
            .servers
            .get(&server_id)
            .ok_or_else(|| format!("Server '{server_id}' not found"))?
            .clone();
        (s, cfg.settings.clone())
    };

    test_connection(&server_cfg, &settings)
        .await
        .map_err(|e| e.to_string())
}

/// Returns true when no config file existed at startup (guides the welcome UI).
#[tauri::command]
pub fn is_first_run(state: State<IsFirstRun>) -> bool {
    state.0
}

/// Returns the absolute path to the running binary, for display in the UI.
#[tauri::command]
pub fn get_binary_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "remote-dev-bridge".to_owned())
}

/// Return a map of server_id → is_connected for the status indicators.
#[tauri::command]
pub async fn get_connection_status(
    config_state: State<'_, Mutex<AppConfig>>,
    sessions: State<'_, SharedSessionManager>,
) -> Result<HashMap<String, bool>, String> {
    let server_ids: Vec<String> = {
        let cfg = config_state.lock().map_err(|e| e.to_string())?;
        cfg.servers.keys().cloned().collect()
    };

    let mgr = sessions.lock().await;
    let result = server_ids
        .into_iter()
        .map(|id| {
            let connected = mgr.get_session(&id).is_ok();
            (id, connected)
        })
        .collect();
    Ok(result)
}
