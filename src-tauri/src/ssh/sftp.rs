use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tracing::debug;

use crate::config::{ServerConfig, Settings};
use crate::error::SshError;
use crate::mcp::tools::shq;

use super::session::SshSession;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub entry_type: EntryType,
    pub size: u64,
    /// Unix timestamp of last modification, if available.
    pub modified: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryType {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileStat {
    pub path: String,
    pub exists: bool,
    pub entry_type: EntryType,
    pub size: u64,
    /// Unix timestamp of last modification, if available.
    pub modified: Option<u64>,
    /// Raw POSIX permission bits (e.g. `0o100644`), if available.
    pub permissions: Option<u32>,
}

// ── SFTP operations ───────────────────────────────────────────────────────────

/// Read a remote file as a UTF-8 string.
///
/// Returns `SshError::FileTooLarge` if the file exceeds `max_size_kb`
/// (pass `0` to disable the limit).
pub async fn sftp_read_file(
    session: &SshSession,
    path: &str,
    max_size_kb: u64,
) -> Result<String, SshError> {
    let sftp = session.open_sftp().await?;

    // Check size before transferring the whole file.
    if max_size_kb > 0 {
        let meta = sftp
            .metadata(path)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))?;
        let size_kb = meta.len() / 1024;
        if size_kb > max_size_kb {
            return Err(SshError::FileTooLarge {
                size_kb,
                limit_kb: max_size_kb,
            });
        }
    }

    let bytes = sftp
        .read(path)
        .await
        .map_err(|e| SshError::Sftp(e.to_string()))?;

    String::from_utf8(bytes).map_err(|_| SshError::NotUtf8)
}

/// Write `content` to a remote file, creating parent directories as needed.
///
/// Returns the number of bytes written.
pub async fn sftp_write_file(
    session: &SshSession,
    path: &str,
    content: &str,
) -> Result<u64, SshError> {
    // Ensure the parent directory exists before opening the SFTP channel so
    // we don't end up with two concurrent channels unnecessarily.
    if let Some(parent) = std::path::Path::new(path).parent() {
        let parent_str = parent.to_string_lossy();
        if !parent_str.is_empty() && parent_str != "." {
            let (_, stderr, code) = session
                .exec(&format!("mkdir -p {}", shq(&parent_str)))
                .await?;
            if code != 0 {
                return Err(SshError::CommandFailed { code, stderr });
            }
        }
    }

    let sftp = session.open_sftp().await?;
    let data = content.as_bytes();

    // Use create() (CREATE | TRUNCATE | WRITE) instead of write() (WRITE only),
    // because write() fails with SSH_FX_NO_SUCH_FILE on new files.
    let mut file = sftp
        .create(path)
        .await
        .map_err(|e| SshError::Sftp(format!("create file: {e}")))?;

    file.write_all(data)
        .await
        .map_err(|e| SshError::Sftp(format!("write data: {e}")))?;

    file.shutdown()
        .await
        .map_err(|e| SshError::Sftp(format!("close file: {e}")))?;

    Ok(data.len() as u64)
}

/// List the entries in a remote directory.
pub async fn sftp_list_dir(
    session: &SshSession,
    path: &str,
) -> Result<Vec<DirEntry>, SshError> {
    let sftp = session.open_sftp().await?;

    let read_dir = sftp
        .read_dir(path)
        .await
        .map_err(|e| SshError::Sftp(e.to_string()))?;

    let entries = read_dir
        .map(|entry| {
            let meta = entry.metadata();
            DirEntry {
                name: entry.file_name(),
                entry_type: attrs_to_entry_type(meta.is_dir(), meta.is_symlink()),
                size: meta.len(),
                modified: meta.mtime.map(|t| t as u64),
            }
        })
        .collect();

    Ok(entries)
}

/// Create a remote directory, including all parents (like `mkdir -p`).
///
/// Uses an SSH exec channel because SFTP `create_dir` is not recursive.
pub async fn sftp_mkdir(session: &SshSession, path: &str) -> Result<(), SshError> {
    let (_, stderr, code) = session.exec(&format!("mkdir -p {}", shq(path))).await?;
    if code != 0 {
        return Err(SshError::CommandFailed { code, stderr });
    }
    Ok(())
}

/// Retrieve metadata for a remote path.
///
/// If the path does not exist, returns `FileStat { exists: false, .. }` rather
/// than an error, so callers can cleanly distinguish "not found" from I/O failures.
pub async fn sftp_stat(session: &SshSession, path: &str) -> Result<FileStat, SshError> {
    let sftp = session.open_sftp().await?;

    match sftp.metadata(path).await {
        Ok(meta) => Ok(FileStat {
            path: path.to_owned(),
            exists: true,
            entry_type: attrs_to_entry_type(meta.is_dir(), meta.is_symlink()),
            size: meta.len(),
            modified: meta.mtime.map(|t| t as u64),
            permissions: meta.permissions,
        }),
        Err(e) => {
            let msg = e.to_string();
            // SFTP status 2 == SSH_FX_NO_SUCH_FILE
            if msg.contains("No such file")
                || msg.contains("not found")
                || msg.contains("SSH_FX_NO_SUCH_FILE")
                || msg.contains("status 2")
            {
                Ok(FileStat {
                    path: path.to_owned(),
                    exists: false,
                    entry_type: EntryType::File,
                    size: 0,
                    modified: None,
                    permissions: None,
                })
            } else {
                Err(SshError::Sftp(msg))
            }
        }
    }
}

/// Delete a remote file or directory.
///
/// For directories, `recursive = false` will fail if the directory is
/// non-empty (SFTP `remove_dir` semantics).  `recursive = true` uses
/// `rm -rf` via an exec channel.
///
/// Returns a list of paths that were removed (currently always just `[path]`).
pub async fn sftp_remove(
    session: &SshSession,
    path: &str,
    recursive: bool,
) -> Result<Vec<String>, SshError> {
    let sftp = session.open_sftp().await?;

    let meta = sftp
        .metadata(path)
        .await
        .map_err(|e| SshError::Sftp(e.to_string()))?;

    if meta.is_dir() && recursive {
        // Drop the SFTP session before exec so we don't keep an extra channel open.
        drop(sftp);
        debug!("sftp_remove: rm -rf '{path}'");
        let (_, stderr, code) = session.exec(&format!("rm -rf -- {}", shq(path))).await?;
        if code != 0 {
            return Err(SshError::CommandFailed { code, stderr });
        }
    } else if meta.is_dir() {
        sftp.remove_dir(path)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))?;
    } else {
        sftp.remove_file(path)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))?;
    }

    Ok(vec![path.to_owned()])
}

/// Rename / move a remote path.
pub async fn sftp_rename(
    session: &SshSession,
    source: &str,
    destination: &str,
) -> Result<(), SshError> {
    let sftp = session.open_sftp().await?;
    sftp.rename(source, destination)
        .await
        .map_err(|e| SshError::Sftp(e.to_string()))
}

// ── SSH exec helper ───────────────────────────────────────────────────────────

/// Execute an arbitrary shell command.  Returns `(stdout, stderr, exit_code)`.
pub async fn ssh_exec(
    session: &SshSession,
    command: &str,
) -> Result<(String, String, i32), SshError> {
    session.exec(command).await
}

// ── Connection test ───────────────────────────────────────────────────────────

/// Quickly verify that we can connect to `config` and run a command.
///
/// Returns a human-readable string with the remote hostname and OS info, or an
/// error describing what went wrong.
pub async fn test_connection(
    config: &ServerConfig,
    settings: &Settings,
) -> Result<String, SshError> {
    let session = SshSession::connect("__test__", config, settings).await?;
    let (stdout, stderr, code) = session
        .exec("echo 'ok' && uname -a && hostname")
        .await?;
    session.disconnect().await;

    if code != 0 {
        return Err(SshError::CommandFailed { code, stderr });
    }
    Ok(stdout.trim().to_owned())
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn attrs_to_entry_type(is_dir: bool, is_symlink: bool) -> EntryType {
    if is_symlink {
        EntryType::Symlink
    } else if is_dir {
        EntryType::Dir
    } else {
        EntryType::File
    }
}
