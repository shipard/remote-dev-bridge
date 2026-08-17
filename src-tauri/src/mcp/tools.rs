use std::time::Duration;

use serde_json::{json, Value};
use tracing::{debug, info};

use crate::config::AppConfig;
use crate::error::SshError;
use crate::ssh::{
    sftp_list_dir, sftp_mkdir, sftp_read_file, sftp_remove, sftp_rename, sftp_stat,
    sftp_write_file, SharedSessionManager,
};

use super::types::{ToolDefinition, ToolResult};

/// Hard cap on the size of any single tool response body.
const MAX_OUTPUT_BYTES: usize = 100 * 1024; // 100 KB

/// Number of characters echoed back in "search string not found" diagnostics.
const SEARCH_PREVIEW_CHARS: usize = 120;

// ── Tool registry ─────────────────────────────────────────────────────────────

pub fn all_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "list_projects",
            description: "List all configured remote development projects.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "read_file",
            description: "Read a file from a remote project. Path is relative to the project root.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (key in config)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path relative to the project root"
                    }
                },
                "required": ["project_id", "path"]
            }),
        },
        ToolDefinition {
            name: "list_directory",
            description: "List files and directories in a remote project path.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path relative to project root (empty string for root)"
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "List recursively (uses find via SSH exec)",
                        "default": false
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Maximum recursion depth (only used when recursive=true)",
                        "default": 3
                    }
                },
                "required": ["project_id"]
            }),
        },
        ToolDefinition {
            name: "write_file",
            description: "Write content to a file in a remote project. Creates parent directories automatically.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path relative to the project root"
                    },
                    "content": {
                        "type": "string",
                        "description": "File content to write"
                    }
                },
                "required": ["project_id", "path", "content"]
            }),
        },
        ToolDefinition {
            name: "create_directory",
            description: "Create a directory (and parent directories) in a remote project.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project identifier" },
                    "path": { "type": "string", "description": "Path relative to project root" }
                },
                "required": ["project_id", "path"]
            }),
        },
        ToolDefinition {
            name: "file_info",
            description: "Get metadata about a remote file or directory (existence, type, size, permissions).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project identifier" },
                    "path": { "type": "string", "description": "Path relative to project root" }
                },
                "required": ["project_id", "path"]
            }),
        },
        ToolDefinition {
            name: "search_files",
            description: "Find files matching a glob pattern inside a remote project.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project identifier" },
                    "pattern": {
                        "type": "string",
                        "description": "Filename glob pattern (e.g. '*.rs', 'main.*')"
                    },
                    "path": {
                        "type": "string",
                        "description": "Sub-directory to search within (empty = project root)"
                    }
                },
                "required": ["project_id", "pattern"]
            }),
        },
        ToolDefinition {
            name: "read_multiple_files",
            description: "Read several files in one call. Continues on per-file errors.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project identifier" },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Relative paths to read"
                    }
                },
                "required": ["project_id", "paths"]
            }),
        },
        ToolDefinition {
            name: "move_path",
            description: "Move or rename a file or directory within a remote project.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project identifier" },
                    "source": { "type": "string", "description": "Source path (relative)" },
                    "destination": { "type": "string", "description": "Destination path (relative)" }
                },
                "required": ["project_id", "source", "destination"]
            }),
        },
        ToolDefinition {
            name: "delete_path",
            description: "Delete a file or directory in a remote project.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project identifier" },
                    "path": { "type": "string", "description": "Path to delete (relative)" },
                    "recursive": {
                        "type": "boolean",
                        "description": "Delete non-empty directories recursively",
                        "default": false
                    }
                },
                "required": ["project_id", "path"]
            }),
        },
        ToolDefinition {
            name: "patch_file",
            description: "Apply search-and-replace edits to a remote file. Fails if any search string is not found.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project identifier" },
                    "path": { "type": "string", "description": "File to patch (relative)" },
                    "edits": {
                        "type": "array",
                        "description": "Ordered list of edits to apply",
                        "items": {
                            "type": "object",
                            "properties": {
                                "search":  { "type": "string", "description": "Exact text to find" },
                                "replace": { "type": "string", "description": "Replacement text" }
                            },
                            "required": ["search", "replace"]
                        }
                    }
                },
                "required": ["project_id", "path", "edits"]
            }),
        },
        ToolDefinition {
            name: "run_command",
            description: "Execute a shell command in the project root directory. Requires allow_commands=true in project config.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project identifier" },
                    "command": { "type": "string", "description": "Shell command to run" },
                    "timeout_sec": {
                        "type": "integer",
                        "description": "Timeout in seconds (default 30, max 120)",
                        "default": 30
                    }
                },
                "required": ["project_id", "command"]
            }),
        },
    ]
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

/// Dispatch an inbound `tools/call` to the correct handler.
pub async fn dispatch(
    tool_name: &str,
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    debug!(tool = tool_name, "dispatching tool call");

    match tool_name {
        "list_projects" => handle_list_projects(config).await,
        "read_file" => handle_read_file(args, config, sessions).await,
        "list_directory" => handle_list_directory(args, config, sessions).await,
        "write_file" => handle_write_file(args, config, sessions).await,
        "create_directory" => handle_create_directory(args, config, sessions).await,
        "file_info" => handle_file_info(args, config, sessions).await,
        "search_files" => handle_search_files(args, config, sessions).await,
        "read_multiple_files" => handle_read_multiple_files(args, config, sessions).await,
        "move_path" => handle_move_path(args, config, sessions).await,
        "delete_path" => handle_delete_path(args, config, sessions).await,
        "patch_file" => handle_patch_file(args, config, sessions).await,
        "run_command" => handle_run_command(args, config, sessions).await,
        other => ToolResult::error(format!("Unknown tool: {other}")),
    }
}

// ── list_projects ─────────────────────────────────────────────────────────────

async fn handle_list_projects(config: &AppConfig) -> ToolResult {
    if config.projects.is_empty() {
        return ToolResult::text("No projects configured. Edit your config file to add projects.");
    }

    let mut lines = vec!["Configured projects:".to_owned()];
    let mut ids: Vec<_> = config.projects.keys().collect();
    ids.sort();
    for id in ids {
        let p = &config.projects[id];
        lines.push(format!(
            "  {id} — {} (server: {}, root: {})",
            p.label, p.server, p.root_path
        ));
        if !p.description.is_empty() {
            lines.push(format!("      {}", p.description));
        }
    }
    ToolResult::text(lines.join("\n"))
}

// ── read_file ─────────────────────────────────────────────────────────────────

async fn handle_read_file(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let rel_path = match required_str(args, "path") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };

    let (session, abs_path) = match resolve(project_id, rel_path, config, sessions).await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(e),
    };

    match sftp_read_file(
        &session,
        &abs_path,
        config.settings.max_file_size_kb,
    )
    .await
    {
        Ok(content) => {
            info!(project = project_id, path = rel_path, "read_file ok");
            ToolResult::text(content)
        }
        Err(e) => ToolResult::error(format!("read_file failed: {e}")),
    }
}

// ── list_directory ────────────────────────────────────────────────────────────

async fn handle_list_directory(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let rel_path = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let recursive = args.get("recursive").and_then(Value::as_bool).unwrap_or(false);
    let max_depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(3);

    let (session, abs_path) = match resolve(project_id, rel_path, config, sessions).await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(e),
    };

    if recursive {
        // Use `find` over exec for recursive listing.
        let cmd = format!(
            "find {} -maxdepth {} -not -path '*/.*' | sort",
            shq(&abs_path),
            max_depth
        );
        match session.exec(&cmd).await {
            Ok((stdout, _, 0)) => {
                let output = if stdout.trim().is_empty() {
                    "(empty directory)".to_owned()
                } else {
                    truncate_output(stdout, MAX_OUTPUT_BYTES)
                };
                ToolResult::text(output)
            }
            Ok((_, stderr, code)) => {
                ToolResult::error(format!("find exited {code}: {stderr}"))
            }
            Err(e) => ToolResult::error(format!("exec failed: {e}")),
        }
    } else {
        match sftp_list_dir(&session, &abs_path).await {
            Ok(entries) => {
                if entries.is_empty() {
                    return ToolResult::text("(empty directory)");
                }
                let mut lines = Vec::with_capacity(entries.len());
                for e in &entries {
                    let type_char = match e.entry_type {
                        crate::ssh::EntryType::Dir => 'd',
                        crate::ssh::EntryType::Symlink => 'l',
                        _ => 'f',
                    };
                    lines.push(format!("[{type_char}] {} ({} bytes)", e.name, e.size));
                }
                lines.sort();
                ToolResult::text(lines.join("\n"))
            }
            Err(e) => ToolResult::error(format!("list_directory failed: {e}")),
        }
    }
}

// ── write_file ────────────────────────────────────────────────────────────────

async fn handle_write_file(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let rel_path = match required_str(args, "path") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let content = match required_str(args, "content") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };

    let (session, abs_path) = match resolve(project_id, rel_path, config, sessions).await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(e),
    };

    match sftp_write_file(&session, &abs_path, content).await {
        Ok(bytes) => {
            info!(project = project_id, path = rel_path, bytes, "write_file ok");
            ToolResult::text(format!("Written {bytes} bytes to {rel_path}"))
        }
        Err(e) => ToolResult::error(format!("write_file failed: {e}")),
    }
}

// ── create_directory ──────────────────────────────────────────────────────────

async fn handle_create_directory(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let rel_path = match required_str(args, "path") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };

    let (session, abs_path) = match resolve(project_id, rel_path, config, sessions).await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(e),
    };

    match sftp_mkdir(&session, &abs_path).await {
        Ok(()) => {
            info!(project = project_id, path = rel_path, "create_directory ok");
            ToolResult::text(format!("Directory created: {rel_path}"))
        }
        Err(e) => ToolResult::error(format!("create_directory failed: {e}")),
    }
}

// ── file_info ─────────────────────────────────────────────────────────────────

async fn handle_file_info(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let rel_path = match required_str(args, "path") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };

    let (session, abs_path) = match resolve(project_id, rel_path, config, sessions).await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(e),
    };

    match sftp_stat(&session, &abs_path).await {
        Ok(stat) => {
            if !stat.exists {
                return ToolResult::text(format!("Path does not exist: {rel_path}"));
            }
            let type_str = match stat.entry_type {
                crate::ssh::EntryType::Dir => "directory",
                crate::ssh::EntryType::Symlink => "symlink",
                crate::ssh::EntryType::File => "file",
                crate::ssh::EntryType::Other => "other",
            };
            let modified = stat
                .modified
                .map(|t| format!("{t}"))
                .unwrap_or_else(|| "unknown".to_owned());
            let perms = stat
                .permissions
                .map(|p| format!("{:o}", p & 0o7777))
                .unwrap_or_else(|| "unknown".to_owned());
            ToolResult::text(format!(
                "path: {rel_path}\ntype: {type_str}\nsize: {} bytes\nmodified: {modified}\npermissions: {perms}",
                stat.size
            ))
        }
        Err(e) => ToolResult::error(format!("file_info failed: {e}")),
    }
}

// ── search_files ──────────────────────────────────────────────────────────────

async fn handle_search_files(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let pattern = match required_str(args, "pattern") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let sub_path = args.get("path").and_then(Value::as_str).unwrap_or("");

    let (session, search_root) = match resolve(project_id, sub_path, config, sessions).await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(e),
    };

    // Grab the project root to compute relative paths in output.
    let project_root = config
        .projects
        .get(project_id)
        .map(|p| p.root_path.trim_end_matches('/').to_owned())
        .unwrap_or_default();

    // Both the root and the pattern are shell-quoted, so no character in them
    // can escape into the command.
    let cmd = format!(
        "find {} -maxdepth 5 -name {} | sort | head -101",
        shq(&search_root),
        shq(pattern)
    );

    match session.exec(&cmd).await {
        Ok((stdout, _, _)) => {
            let lines: Vec<&str> = stdout.lines().collect();
            let truncated = lines.len() > 100;
            let results: Vec<String> = lines
                .iter()
                .take(100)
                .map(|l| {
                    // Strip the absolute project root prefix → relative path.
                    l.strip_prefix(&project_root)
                        .unwrap_or(l)
                        .trim_start_matches('/')
                        .to_owned()
                })
                .filter(|l| !l.is_empty())
                .collect();

            if results.is_empty() {
                return ToolResult::text(format!("No files matching '{pattern}' found."));
            }

            let mut out = results.join("\n");
            if truncated {
                out.push_str("\n(results truncated at 100 — refine your pattern or path)");
            }
            ToolResult::text(out)
        }
        Err(e) => ToolResult::error(format!("search_files failed: {e}")),
    }
}

// ── read_multiple_files ───────────────────────────────────────────────────────

async fn handle_read_multiple_files(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let paths = match args.get("paths").and_then(Value::as_array) {
        Some(a) => a,
        None => return ToolResult::error("Missing required field: 'paths'"),
    };

    if paths.is_empty() {
        return ToolResult::error("'paths' must not be empty");
    }

    // Establish the session once using an empty relative path (project root).
    let (session, _) = match resolve(project_id, "", config, sessions).await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(e),
    };

    let project = config.projects.get(project_id).unwrap(); // safe — resolve succeeded
    let max_kb = config.settings.max_file_size_kb;

    let mut sections: Vec<String> = Vec::with_capacity(paths.len());

    for entry in paths {
        let rel_path = match entry.as_str() {
            Some(s) => s,
            None => {
                sections.push("=== (invalid path entry — skipped) ===\n".to_owned());
                continue;
            }
        };

        let section = match resolve_safe_path(&project.root_path, rel_path) {
            Err(e) => format!("=== {rel_path} ===\nERROR: {e}\n"),
            Ok(abs_path) => match sftp_read_file(&session, &abs_path, max_kb).await {
                Ok(content) => format!("=== {rel_path} ===\n{content}\n"),
                Err(e) => format!("=== {rel_path} ===\nERROR: {e}\n"),
            },
        };
        sections.push(section);
    }

    ToolResult::text(sections.join("\n"))
}

// ── move_path ─────────────────────────────────────────────────────────────────

async fn handle_move_path(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let src_rel = match required_str(args, "source") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let dst_rel = match required_str(args, "destination") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };

    // Both paths must pass security validation.
    let (session, src_abs) = match resolve(project_id, src_rel, config, sessions).await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(e),
    };
    let project = config.projects.get(project_id).unwrap();
    let dst_abs = match resolve_safe_path(&project.root_path, dst_rel) {
        Ok(p) => p,
        Err(e) => return ToolResult::error(format!("Destination path error: {e}")),
    };

    match sftp_rename(&session, &src_abs, &dst_abs).await {
        Ok(()) => {
            info!(project = project_id, from = src_rel, to = dst_rel, "move_path ok");
            ToolResult::text(format!("Moved: {src_rel} → {dst_rel}"))
        }
        Err(e) => ToolResult::error(format!("move_path failed: {e}")),
    }
}

// ── delete_path ───────────────────────────────────────────────────────────────

async fn handle_delete_path(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let rel_path = match required_str(args, "path") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let recursive = args.get("recursive").and_then(Value::as_bool).unwrap_or(false);

    let (session, abs_path) = match resolve(project_id, rel_path, config, sessions).await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(e),
    };

    match sftp_remove(&session, &abs_path, recursive).await {
        Ok(removed) => {
            info!(project = project_id, path = rel_path, "delete_path ok");
            ToolResult::text(format!("Deleted {} path(s): {}", removed.len(), removed.join(", ")))
        }
        Err(e) => ToolResult::error(format!("delete_path failed: {e}")),
    }
}

// ── patch_file ────────────────────────────────────────────────────────────────

async fn handle_patch_file(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let rel_path = match required_str(args, "path") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let edits = match args.get("edits").and_then(Value::as_array) {
        Some(a) => a,
        None => return ToolResult::error("Missing required field: 'edits'"),
    };

    if edits.is_empty() {
        return ToolResult::error("'edits' must not be empty");
    }

    let (session, abs_path) = match resolve(project_id, rel_path, config, sessions).await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(e),
    };

    // Read the file.
    let mut content = match sftp_read_file(&session, &abs_path, config.settings.max_file_size_kb).await {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("patch_file: read failed: {e}")),
    };

    // Apply edits in order, failing fast if any search string is missing.
    for (i, edit) in edits.iter().enumerate() {
        let search = match edit.get("search").and_then(Value::as_str) {
            Some(s) => s,
            None => return ToolResult::error(format!("Edit #{i}: missing 'search' field")),
        };
        let replace = match edit.get("replace").and_then(Value::as_str) {
            Some(r) => r,
            None => return ToolResult::error(format!("Edit #{i}: missing 'replace' field")),
        };

        if search.is_empty() {
            return ToolResult::error(format!("Edit #{i}: 'search' must not be empty"));
        }
        if !content.contains(search) {
            return ToolResult::error(format!(
                "Edit #{i}: search string not found in file — aborting to prevent partial edits.\n\
                 Search string (first {SEARCH_PREVIEW_CHARS} chars): {:?}",
                preview(search, SEARCH_PREVIEW_CHARS)
            ));
        }
        content = content.replacen(search, replace, 1);
    }

    // Write back.
    match sftp_write_file(&session, &abs_path, &content).await {
        Ok(bytes) => {
            info!(project = project_id, path = rel_path, edits = edits.len(), "patch_file ok");
            ToolResult::text(format!(
                "Applied {} edit(s) to {rel_path} ({bytes} bytes written)",
                edits.len()
            ))
        }
        Err(e) => ToolResult::error(format!("patch_file: write failed: {e}")),
    }
}

// ── run_command ───────────────────────────────────────────────────────────────

async fn handle_run_command(
    args: &Value,
    config: &AppConfig,
    sessions: &SharedSessionManager,
) -> ToolResult {
    let project_id = match required_str(args, "project_id") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let command = match required_str(args, "command") {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let timeout_sec = args
        .get("timeout_sec")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .min(120);

    // Gate: project must have allow_commands = true.
    let project = match config.projects.get(project_id) {
        Some(p) => p,
        None => return ToolResult::error(format!("Project '{project_id}' not found")),
    };

    if !project.allow_commands {
        return ToolResult::error(format!(
            "Command execution is disabled for project '{project_id}'. \
             Set allow_commands=true in config to enable."
        ));
    }

    // If an allowlist is configured, verify the command starts with a permitted prefix.
    if let Some(allowed) = &project.allowed_commands {
        if !allowed.is_empty() {
            // Require a word boundary after the prefix, otherwise an allowed
            // `git` would also permit `gitfoo; rm -rf ~`.
            let permitted = allowed.iter().any(|prefix| {
                command == prefix.as_str()
                    || command
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
            });
            if !permitted {
                return ToolResult::error(format!(
                    "Command not in allowed list for '{project_id}'. \
                     Allowed prefixes: {}",
                    allowed.join(", ")
                ));
            }
        }
    }

    // Ensure SSH session.
    let server_cfg = match config.servers.get(&project.server) {
        Some(s) => s,
        None => return ToolResult::error(format!("Server '{}' not found", project.server)),
    };
    {
        let mut mgr = sessions.lock().await;
        if let Err(e) = mgr.ensure_connected(&project.server, server_cfg).await {
            return ToolResult::error(format!("SSH connect failed: {e}"));
        }
    }
    let session = {
        let mgr = sessions.lock().await;
        match mgr.get_session(&project.server) {
            Ok(s) => s,
            Err(e) => return ToolResult::error(format!("No live session: {e}")),
        }
    };

    // Wrap in `cd <root> && <command>` to set working directory.
    let root = project.root_path.trim_end_matches('/');
    let full_cmd = format!("cd {} && {}", shq(root), command);

    let exec_fut = session.exec(&full_cmd);
    let result = tokio::time::timeout(Duration::from_secs(timeout_sec), exec_fut).await;

    match result {
        Err(_) => ToolResult::error(format!(
            "Command timed out after {timeout_sec}s"
        )),
        Ok(Err(e)) => ToolResult::error(format!("exec failed: {e}")),
        Ok(Ok((stdout, stderr, code))) => {
            let stdout = truncate_output(stdout, MAX_OUTPUT_BYTES);
            let stderr = truncate_output(stderr, MAX_OUTPUT_BYTES);

            info!(project = project_id, exit_code = code, "run_command ok");

            let mut out = format!("exit_code: {code}\n");
            if !stdout.is_empty() {
                out.push_str(&format!("\nstdout:\n{stdout}"));
            }
            if !stderr.is_empty() {
                out.push_str(&format!("\nstderr:\n{stderr}"));
            }

            if code == 0 {
                ToolResult::text(out)
            } else {
                ToolResult::error(out)
            }
        }
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Look up the project, ensure SSH is connected, resolve and security-check
/// the path. Returns `(Arc<SshSession>, absolute_remote_path)`.
async fn resolve<'a>(
    project_id: &'a str,
    rel_path: &'a str,
    config: &'a AppConfig,
    sessions: &SharedSessionManager,
) -> Result<(std::sync::Arc<crate::ssh::SshSession>, String), String> {
    // Look up project.
    let project = config
        .projects
        .get(project_id)
        .ok_or_else(|| format!("Project '{project_id}' not found in config"))?;

    // Look up server config.
    let server_cfg = config
        .servers
        .get(&project.server)
        .ok_or_else(|| format!("Server '{}' not found in config", project.server))?;

    // Path security check BEFORE opening any SSH connection — reject traversal
    // attempts early so they never touch the network.
    let abs_path = resolve_safe_path(&project.root_path, rel_path)
        .map_err(|e| format!("Path error: {e}"))?;

    // Ensure SSH session (connects lazily on first use).
    {
        let mut mgr = sessions.lock().await;
        mgr.ensure_connected(&project.server, server_cfg)
            .await
            .map_err(|e: SshError| format!("SSH connect failed: {e}"))?;
    }

    // Get the live session.
    let session = {
        let mgr = sessions.lock().await;
        mgr.get_session(&project.server)
            .map_err(|e: SshError| format!("No live session: {e}"))?
    };

    Ok((session, abs_path))
}

/// Resolve `relative` against `root` and reject any traversal attempt.
///
/// Rules:
/// - `relative` must not be absolute (start with `/`)
/// - The normalised result must start with `root` (no `..` escape)
pub fn resolve_safe_path(root: &str, relative: &str) -> Result<String, String> {
    if relative.starts_with('/') {
        return Err(format!(
            "Absolute paths are not allowed (got '{relative}')"
        ));
    }

    // Normalise by processing each component.
    let base: Vec<&str> = root
        .trim_end_matches('/')
        .split('/')
        .filter(|c| !c.is_empty())
        .collect();

    let mut parts: Vec<&str> = base.clone();

    for component in relative.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if parts.len() <= base.len() {
                    // Would escape the root.
                    return Err(format!(
                        "Path traversal detected: '{relative}' escapes project root '{root}'"
                    ));
                }
                parts.pop();
            }
            c => parts.push(c),
        }
    }

    Ok(format!("/{}", parts.join("/")))
}

/// Extract a required string field from a JSON args object.
fn required_str<'a>(args: &'a Value, field: &'static str) -> Result<&'a str, String> {
    args.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing required field: '{field}'"))
}

/// Take at most `max_chars` *characters* (never bytes) from `s`.
///
/// Byte slicing (`&s[..120]`) panics when the cut lands inside a multi-byte
/// UTF-8 character - Czech diacritics, typographic quotes, em dashes, emoji.
/// This never panics.
fn preview(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (n, ch) in s.chars().enumerate() {
        if n == max_chars {
            out.push('\u{2026}');
            break;
        }
        out.push(ch);
    }
    out
}

/// Largest index `<= max_bytes` that is a valid UTF-8 character boundary.
///
/// Local implementation instead of `str::floor_char_boundary` so the crate does
/// not depend on the toolchain version.
fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Truncate output to at most `max_bytes`, appending a notice if clipped.
fn truncate_output(mut s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let cut = floor_char_boundary(&s, max_bytes);
    s.truncate(cut);
    s.push_str("\n[... output truncated ...]");
    s
}

/// POSIX-quote a string for safe interpolation into a shell command.
///
/// `it's` becomes `'it'\''s'`. Paths and patterns reach us from the model and
/// may contain anything, so every shell interpolation site must use this.
pub fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard: the exact shape that used to panic.
    /// 119 ASCII bytes, then a 3-byte typographic quote straddling byte 120.
    #[test]
    fn preview_does_not_panic_on_multibyte_at_the_cut() {
        let s = format!("{}„český text“ pokračuje dál", "a".repeat(119));
        let p = preview(&s, SEARCH_PREVIEW_CHARS);
        assert!(p.chars().count() <= SEARCH_PREVIEW_CHARS + 1);
        assert!(p.ends_with('…'));
    }

    #[test]
    fn preview_short_input_is_unchanged() {
        assert_eq!(preview("žluťoučký kůň", 120), "žluťoučký kůň");
    }

    #[test]
    fn preview_at_exact_boundary_has_no_ellipsis() {
        assert_eq!(preview("ábc", 3), "ábc");
    }

    #[test]
    fn floor_char_boundary_never_splits_a_char() {
        let s = "ááááá"; // 2 bytes each
        for n in 0..=s.len() + 2 {
            let i = floor_char_boundary(s, n);
            assert!(s.is_char_boundary(i), "index {i} is not a char boundary");
        }
    }

    #[test]
    fn truncate_output_is_utf8_safe() {
        let s = "ě".repeat(100); // 200 bytes
        let out = truncate_output(s, 51); // 51 is mid-character
        assert!(out.starts_with(&"ě".repeat(25)));
        assert!(out.ends_with("[... output truncated ...]"));
    }

    #[test]
    fn shq_escapes_single_quotes() {
        assert_eq!(shq("Patrik's notes.md"), r"'Patrik'\''s notes.md'");
        assert_eq!(shq("plain"), "'plain'");
        assert_eq!(shq("a'; rm -rf ~; echo '"), r"'a'\''; rm -rf ~; echo '\'''");
    }

    #[test]
    fn allowlist_requires_a_word_boundary() {
        let allowed = ["git".to_string()];
        let check = |command: &str| {
            allowed.iter().any(|prefix| {
                command == prefix.as_str()
                    || command
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
            })
        };
        assert!(check("git status"));
        assert!(check("git"));
        assert!(!check("gitfoo; rm -rf ~"));
    }

    #[test]
    fn resolve_safe_path_blocks_traversal() {
        assert!(resolve_safe_path("/home/u/proj", "../../etc/passwd").is_err());
        assert!(resolve_safe_path("/home/u/proj", "/etc/passwd").is_err());
        assert_eq!(
            resolve_safe_path("/home/u/proj", "src/main.rs").unwrap(),
            "/home/u/proj/src/main.rs"
        );
    }
}
