use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use russh::client::{self, AuthResult};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use russh_sftp::client::SftpSession;
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::config::{ServerConfig, Settings};
use crate::error::SshError;

// ── Handler ──────────────────────────────────────────────────────────────────

/// Minimal SSH client handler.
///
/// For now, accepts every server host key. TODO: add known-hosts verification
/// before this becomes production-facing.
struct ClientHandler;

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Accept all keys — safe enough for a tool that targets explicitly
        // configured dev servers, but should be hardened later.
        Ok(true)
    }
}

// ── SshSession ───────────────────────────────────────────────────────────────

pub struct SshSession {
    pub server_id: String,
    /// The underlying russh Handle.  We wrap it in a Mutex because:
    ///   - `authenticate_publickey` needs `&mut Handle`
    ///   - `channel_open_session` needs only `&Handle`, but we lock briefly
    ///     just to obtain the channel then immediately release the lock so
    ///     long-running I/O doesn't block other callers.
    handle: Mutex<client::Handle<ClientHandler>>,
}

impl SshSession {
    /// Establish a new SSH connection and authenticate with the key specified
    /// in `config`.
    pub async fn connect(
        server_id: &str,
        config: &ServerConfig,
        settings: &Settings,
    ) -> Result<Self, SshError> {
        let timeout_sec = settings.ssh_connection_timeout_sec;

        let client_config = Arc::new(client::Config {
            keepalive_interval: Some(Duration::from_secs(settings.ssh_keepalive_interval_sec)),
            keepalive_max: 3,
            ..Default::default()
        });

        // TCP connect with timeout.
        let addr = (config.host.as_str(), config.port);
        let mut handle = tokio::time::timeout(
            Duration::from_secs(timeout_sec),
            client::connect(client_config, addr, ClientHandler),
        )
        .await
        .map_err(|_| SshError::Timeout {
            host: config.host.clone(),
            port: config.port,
        })?
        .map_err(|e| SshError::ConnectionFailed {
            host: config.host.clone(),
            reason: e.to_string(),
        })?;

        // Load private key and authenticate.
        let key_path = expand_tilde(&config.auth.key_path);
        let key = load_secret_key(&key_path, None).map_err(|e| SshError::KeyLoad {
            path: key_path.display().to_string(),
            reason: e.to_string(),
        })?;

        // RSA keys need an explicit hash algorithm for rsa-sha2-256 negotiation.
        // Ed25519 and ECDSA keys ignore this parameter.
        let hash_alg = match key.algorithm() {
            russh::keys::ssh_key::Algorithm::Rsa { .. } => {
                Some(russh::keys::ssh_key::HashAlg::Sha256)
            }
            _ => None,
        };
        let key_with_alg = PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg);

        let auth_result = handle
            .authenticate_publickey(&config.username, key_with_alg)
            .await
            .map_err(|e| SshError::Session(e.to_string()))?;

        if !matches!(auth_result, AuthResult::Success) {
            return Err(SshError::AuthFailed {
                user: config.username.clone(),
            });
        }

        info!(
            server_id,
            "{}@{}:{} → SSH authenticated",
            config.username,
            config.host,
            config.port
        );

        Ok(Self {
            server_id: server_id.to_owned(),
            handle: Mutex::new(handle),
        })
    }

    /// Open a fresh SFTP subsystem on a new channel.
    ///
    /// The SFTP session owns its channel; caller drops it when done.
    pub async fn open_sftp(&self) -> Result<SftpSession, SshError> {
        let channel = {
            let guard = self.handle.lock().await;
            guard
                .channel_open_session()
                .await
                .map_err(|e| SshError::Session(format!("open channel: {e}")))?
        };
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| SshError::Session(format!("request sftp subsystem: {e}")))?;
        SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))
    }

    /// Run a shell command, returning `(stdout, stderr, exit_code)`.
    pub async fn exec(&self, command: &str) -> Result<(String, String, i32), SshError> {
        let mut channel = {
            let guard = self.handle.lock().await;
            guard
                .channel_open_session()
                .await
                .map_err(|e| SshError::Session(format!("open channel: {e}")))?
        };

        channel
            .exec(true, command)
            .await
            .map_err(|e| SshError::Session(format!("exec: {e}")))?;

        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();
        let mut exit_code: i32 = 0;

        loop {
            let Some(msg) = channel.wait().await else {
                break;
            };
            match msg {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, ext: 1 } => stderr.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => exit_code = exit_status as i32,
                ChannelMsg::Eof | ChannelMsg::Close => {}
                _ => {}
            }
        }

        Ok((
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
            exit_code,
        ))
    }

    /// Gracefully close the connection.
    pub async fn disconnect(&self) {
        let guard = self.handle.lock().await;
        if let Err(e) = guard
            .disconnect(Disconnect::ByApplication, "client disconnecting", "en-US")
            .await
        {
            debug!("Disconnect warning (often normal on drop): {e}");
        }
    }

    /// Returns `true` if the underlying SSH connection is no longer usable.
    pub fn is_closed(&self) -> bool {
        match self.handle.try_lock() {
            Ok(guard) => guard.is_closed(),
            Err(_) => false, // assume open if we can't check right now
        }
    }
}

// ── SshSessionManager ────────────────────────────────────────────────────────

/// How long a session may sit unused before we bother probing it.
const IDLE_PROBE_AFTER: Duration = Duration::from_secs(60);
/// How long the probe itself may take before we declare the session dead.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on a courtesy disconnect of a session we already believe is dead.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);

pub struct SshSessionManager {
    sessions: HashMap<String, Arc<SshSession>>,
    /// Last time each session was handed out, used to decide when to probe.
    last_used: HashMap<String, Instant>,
    pub settings: Settings,
}

/// Thread-safe, heap-allocated session manager.
pub type SharedSessionManager = Arc<Mutex<SshSessionManager>>;

impl SshSessionManager {
    pub fn new(settings: Settings) -> Self {
        Self {
            sessions: HashMap::new(),
            last_used: HashMap::new(),
            settings,
        }
    }

    pub fn new_shared(settings: Settings) -> SharedSessionManager {
        Arc::new(Mutex::new(Self::new(settings)))
    }

    /// Connect to a server and store the session.  If a live session already
    /// exists for `server_id`, returns immediately without reconnecting.
    pub async fn connect(
        &mut self,
        server_id: &str,
        config: &ServerConfig,
    ) -> Result<(), SshError> {
        if let Some(existing) = self.sessions.get(server_id) {
            if !existing.is_closed() {
                debug!(server_id, "session already active — skipping reconnect");
                return Ok(());
            }
            self.sessions.remove(server_id);
        }
        let session = SshSession::connect(server_id, config, &self.settings).await?;
        self.sessions.insert(server_id.to_owned(), Arc::new(session));
        Ok(())
    }

    /// Return a live session, or error if not connected / connection is dead.
    pub fn get_session(&self, server_id: &str) -> Result<Arc<SshSession>, SshError> {
        self.sessions
            .get(server_id)
            .filter(|s| !s.is_closed())
            .cloned()
            .ok_or_else(|| SshError::NotConnected {
                server_id: server_id.to_owned(),
            })
    }

    /// Disconnect a specific server and remove its session.
    pub async fn disconnect(&mut self, server_id: &str) -> Result<(), SshError> {
        self.last_used.remove(server_id);
        if let Some(session) = self.sessions.remove(server_id) {
            session.disconnect().await;
        }
        Ok(())
    }

    /// Disconnect all active sessions.
    pub async fn disconnect_all(&mut self) {
        self.last_used.clear();
        let ids: Vec<_> = self.sessions.keys().cloned().collect();
        for id in ids {
            if let Some(s) = self.sessions.remove(&id) {
                s.disconnect().await;
            }
        }
    }

    /// Like `connect`, but also verifies that the session is genuinely alive.
    ///
    /// `is_closed()` does not catch a half-open socket after the laptop sleeps
    /// or the network changes (Wi-Fi <-> LTE): russh keeps reporting the handle
    /// as alive while every subsequent call fails on `open channel`. So after an
    /// idle period we send a cheap `true` and rebuild the session if it does not
    /// come back.
    pub async fn ensure_connected(
        &mut self,
        server_id: &str,
        config: &ServerConfig,
    ) -> Result<(), SshError> {
        let stale = match self.sessions.get(server_id) {
            None => true,
            Some(s) if s.is_closed() => true,
            Some(s) => {
                let idle = self
                    .last_used
                    .get(server_id)
                    .map(|t| t.elapsed())
                    .unwrap_or(IDLE_PROBE_AFTER);

                if idle < IDLE_PROBE_AFTER {
                    false
                } else {
                    match tokio::time::timeout(PROBE_TIMEOUT, s.exec("true")).await {
                        Ok(Ok(_)) => false,
                        Ok(Err(e)) => {
                            info!(server_id, "liveness probe failed ({e}) - reconnecting");
                            true
                        }
                        Err(_) => {
                            info!(server_id, "liveness probe timed out - reconnecting");
                            true
                        }
                    }
                }
            }
        };

        if stale {
            if let Some(old) = self.sessions.remove(server_id) {
                // A half-open socket can block on disconnect - hence the timeout.
                let _ = tokio::time::timeout(DISCONNECT_TIMEOUT, old.disconnect()).await;
            }
            info!(server_id, "establishing fresh SSH session");
            self.connect(server_id, config).await?;
        }

        self.last_used.insert(server_id.to_owned(), Instant::now());
        Ok(())
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Replace a leading `~` with the current user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}
