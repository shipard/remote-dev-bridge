use std::io::{self, BufRead, Write};
use std::time::Duration;

use serde_json::{json, Value};
use tracing::{debug, error, warn};

use crate::config::AppConfig;
use crate::ssh::SharedSessionManager;

use super::tools::{all_tools, dispatch};
use super::types::{
    InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse, ServerCapabilities,
    ServerInfo, ToolResult,
};

/// Outer deadline for a single `tools/call`.
///
/// SFTP operations have no timeout of their own, so without this a stalled
/// channel wedges the whole stdio loop and every later call times out on the
/// Claude Desktop side. `run_command` keeps its own, shorter, caller-supplied
/// timeout.
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(120);

// ── McpServer ─────────────────────────────────────────────────────────────────

pub struct McpServer {
    config_path: std::path::PathBuf,
    sessions: SharedSessionManager,
}

impl McpServer {
    pub fn new(config_path: std::path::PathBuf, sessions: SharedSessionManager) -> Self {
        Self { config_path, sessions }
    }

    /// Reload config from disk. Called on every tools/call so that changes
    /// made via the tray UI are picked up without restarting Claude Desktop.
    fn load_config(&self) -> Result<AppConfig, String> {
        crate::config::load_config_from_path(&self.config_path)
            .map_err(|e| format!("Failed to reload config: {e}"))
    }

    /// Run the stdio JSON-RPC loop.  Reads lines from stdin, dispatches, writes
    /// responses to stdout.  Blocks until stdin is closed or a fatal I/O error.
    pub async fn run(&self) {
        let stdin = io::stdin();
        let stdout = io::stdout();

        for line in stdin.lock().lines() {
            let raw = match line {
                Ok(l) if l.trim().is_empty() => continue,
                Ok(l) => l,
                Err(e) => {
                    error!("stdin read error: {e}");
                    break;
                }
            };

            debug!("← {raw}");

            let response_bytes = self.handle_line(&raw).await;

            if let Some(bytes) = response_bytes {
                let mut out = stdout.lock();
                if let Err(e) = out.write_all(&bytes).and_then(|_| out.flush()) {
                    error!("stdout write error: {e}");
                    break;
                }
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    debug!("→ {}", s.trim_end());
                }
            }
        }
    }

    /// Parse one line and return the serialised response (with trailing newline),
    /// or `None` for notifications that need no reply.
    async fn handle_line(&self, raw: &str) -> Option<Vec<u8>> {
        let req: JsonRpcRequest = match serde_json::from_str(raw) {
            Ok(r) => r,
            Err(e) => {
                // Malformed JSON — return a parse-error response with null id.
                warn!("JSON parse error: {e}");
                return Some(encode(JsonRpcError::new(
                    Value::Null,
                    -32700,
                    format!("Parse error: {e}"),
                )));
            }
        };

        // Notifications must not receive a response.
        // MCP methods under the "notifications/" namespace are always notifications,
        // even when a client mistakenly includes an `id`.
        if req.is_notification() || req.method.starts_with("notifications/") {
            debug!("notification '{}' — no reply", req.method);
            return None;
        }

        let id = req.id.clone().unwrap_or(Value::Null);
        let result = self.dispatch_request(&req.method, req.params.as_ref(), id.clone()).await;
        Some(result)
    }

    async fn dispatch_request(
        &self,
        method: &str,
        params: Option<&Value>,
        id: Value,
    ) -> Vec<u8> {
        match method {
            "initialize" => encode(JsonRpcResponse::ok(id, self.handle_initialize())),

            "tools/list" => {
                let tools: Vec<_> = all_tools();
                encode(JsonRpcResponse::ok(id, json!({ "tools": tools })))
            }

            "tools/call" => {
                let result = self.handle_tools_call(params, id.clone()).await;
                result
            }

            // Requests (not notifications) we don't know → method-not-found.
            other => encode(JsonRpcError::method_not_found(id, other)),
        }
    }

    fn handle_initialize(&self) -> Value {
        serde_json::to_value(InitializeResult {
            protocol_version: "2024-11-05",
            capabilities: ServerCapabilities {
                tools: json!({}),
            },
            server_info: ServerInfo {
                name: "remote-dev-bridge",
                version: env!("CARGO_PKG_VERSION"),
            },
        })
        .unwrap_or(Value::Null)
    }

    async fn handle_tools_call(&self, params: Option<&Value>, id: Value) -> Vec<u8> {
        let params = match params {
            Some(p) => p,
            None => return encode(JsonRpcError::invalid_params(id, "missing params")),
        };

        let tool_name = match params.get("name").and_then(Value::as_str) {
            Some(n) => n,
            None => {
                return encode(JsonRpcError::invalid_params(id, "missing 'name' field"));
            }
        };

        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        let config = match self.load_config() {
            Ok(c) => c,
            Err(e) => return encode(JsonRpcError::internal(id, e)),
        };

        // The tool call runs in its own task and under a deadline, so that
        // neither a panic in a handler nor a stalled SSH channel can take the
        // bridge down. Requires panic = "unwind" (see Cargo.toml).
        let sessions = self.sessions.clone();
        let name = tool_name.to_owned();
        let mut join = tokio::spawn(async move { dispatch(&name, &args, &config, &sessions).await });

        // Poll the handle by reference: on timeout we still own it and can abort.
        // Dropping a JoinHandle only detaches the task, and a detached task keeps
        // holding the SshSessionManager lock, stalling every later call.
        let tool_result: ToolResult = match tokio::time::timeout(TOOL_CALL_TIMEOUT, &mut join).await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) if e.is_panic() => {
                error!("tool handler panicked: {e}");
                ToolResult::error(
                    "Internal error: the tool handler panicked. The bridge is still \
                     alive - retry, and please report this.",
                )
            }
            Ok(Err(e)) => ToolResult::error(format!("Internal error: task failed ({e})")),
            Err(_) => {
                join.abort();
                error!(tool = tool_name, "tool call exceeded the bridge deadline");
                ToolResult::error(format!(
                    "Tool '{tool_name}' exceeded the {}s bridge deadline and was abandoned. \
                     The SSH session will be re-established on the next call.",
                    TOOL_CALL_TIMEOUT.as_secs()
                ))
            }
        };

        encode(JsonRpcResponse::ok(id, serde_json::to_value(tool_result).unwrap_or(Value::Null)))
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn encode<T: serde::Serialize>(v: T) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&v).unwrap_or_else(|_| b"null".to_vec());
    bytes.push(b'\n');
    bytes
}
