use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Config error: {0}")]
    Config(#[from] ConfigError),

    #[error("SSH error: {0}")]
    Ssh(#[from] SshError),

    #[error("MCP error: {0}")]
    Mcp(#[from] McpError),

    #[error("Filesystem error: {0}")]
    Fs(#[from] FsError),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to determine config path")]
    PathResolution,

    #[error("Failed to read config file: {0}")]
    Read(#[from] std::io::Error),

    #[error("Failed to parse config: {0}")]
    Parse(#[from] serde_json::Error),

    #[error("Validation failed: {0}")]
    Validation(String),
}

#[derive(Debug, Error)]
pub enum SshError {
    #[error("Connection to {host}:{port} timed out")]
    Timeout { host: String, port: u16 },

    #[error("Connection failed to {host}: {reason}")]
    ConnectionFailed { host: String, reason: String },

    #[error("Authentication failed for user '{user}'")]
    AuthFailed { user: String },

    #[error("Session '{server_id}' is not connected")]
    NotConnected { server_id: String },

    #[error("Cannot load SSH key from '{path}': {reason}")]
    KeyLoad { path: String, reason: String },

    #[error("File too large: {size_kb}KB exceeds limit of {limit_kb}KB")]
    FileTooLarge { size_kb: u64, limit_kb: u64 },

    #[error("File is not valid UTF-8")]
    NotUtf8,

    #[error("Command failed (exit {code}): {stderr}")]
    CommandFailed { code: i32, stderr: String },

    #[error("SSH session error: {0}")]
    Session(String),

    #[error("SFTP error: {0}")]
    Sftp(String),
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum FsError {
    #[error("Path traversal attempt detected: {0}")]
    PathTraversal(String),

    #[error("Path not allowed: {0}")]
    PathNotAllowed(String),

    #[error("File too large: {size_kb}KB exceeds limit of {limit_kb}KB")]
    FileTooLarge { size_kb: u64, limit_kb: u64 },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
