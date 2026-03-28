use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::ConfigError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub version: u32,
    pub servers: HashMap<String, ServerConfig>,
    pub projects: HashMap<String, ProjectConfig>,
    pub settings: Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub label: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub auth_type: String,
    pub key_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub label: String,
    pub server: String,
    pub root_path: String,
    pub description: String,
    #[serde(default)]
    pub allow_commands: bool,
    pub allowed_commands: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_max_file_size_kb")]
    pub max_file_size_kb: u64,
    #[serde(default = "default_confirm_destructive")]
    pub confirm_destructive: bool,
    #[serde(default = "default_ssh_connection_timeout_sec")]
    pub ssh_connection_timeout_sec: u64,
    #[serde(default = "default_ssh_keepalive_interval_sec")]
    pub ssh_keepalive_interval_sec: u64,
}

fn default_port() -> u16 { 22 }
fn default_max_file_size_kb() -> u64 { 1024 }
fn default_confirm_destructive() -> bool { true }
fn default_ssh_connection_timeout_sec() -> u64 { 10 }
fn default_ssh_keepalive_interval_sec() -> u64 { 30 }

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_file_size_kb: default_max_file_size_kb(),
            confirm_destructive: default_confirm_destructive(),
            ssh_connection_timeout_sec: default_ssh_connection_timeout_sec(),
            ssh_keepalive_interval_sec: default_ssh_keepalive_interval_sec(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            servers: HashMap::new(),
            projects: HashMap::new(),
            settings: Settings::default(),
        }
    }
}

/// Returns the platform-specific config file path.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    let base = dirs::config_dir().ok_or(ConfigError::PathResolution)?;
    Ok(base.join("remote-dev-bridge").join("config.json"))
}

/// Returns true if a config file already exists on disk.
pub fn config_exists() -> bool {
    config_path().map(|p| p.exists()).unwrap_or(false)
}

/// Load config from a specific path.
///
/// Returns `AppConfig::default()` if the file doesn't exist.  If the file
/// exists but is corrupt, backs it up and returns defaults.
pub fn load_config_from_path(path: &std::path::Path) -> Result<AppConfig, ConfigError> {
    if !path.exists() {
        info!("Config not found at {:?}, using default", path);
        return Ok(AppConfig::default());
    }

    debug!("Loading config from {:?}", path);
    let contents = std::fs::read_to_string(path)?;
    match serde_json::from_str::<AppConfig>(&contents) {
        Ok(config) => Ok(config),
        Err(e) => {
            // Back up the corrupt file so the user can recover their data.
            let backup = path.with_extension("json.bak");
            let _ = std::fs::copy(path, &backup);
            tracing::warn!(
                "Config at {:?} is corrupt ({e}); backed up to {:?} and using defaults",
                path,
                backup
            );
            Ok(AppConfig::default())
        }
    }
}

/// Load config from disk, creating a default config if the file doesn't exist.
/// If the file exists but is corrupt, backs it up and returns a fresh default.
pub fn load_config() -> Result<AppConfig, ConfigError> {
    load_config_from_path(&config_path()?)
}

/// Write config to disk atomically (write to temp file then rename).
pub fn save_config(config: &AppConfig) -> Result<(), ConfigError> {
    let path = config_path()?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(config)?;

    // Write to a temp file next to the real file, then rename for atomicity.
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, &path)?;

    debug!("Config saved to {:?}", path);
    Ok(())
}

/// Validate referential integrity: every project.server must reference an
/// existing key in the servers map.
pub fn validate_config(config: &AppConfig) -> Result<(), ConfigError> {
    for (project_key, project) in &config.projects {
        if !config.servers.contains_key(&project.server) {
            return Err(ConfigError::Validation(format!(
                "Project '{}' references unknown server '{}'",
                project_key, project.server
            )));
        }
    }
    Ok(())
}
