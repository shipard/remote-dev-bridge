use std::io::{self, BufRead, Write};

use serde_json::{json, Value};
use tracing::{debug, error, warn};

use crate::config::AppConfig;
use crate::ssh::SharedSessionManager;

use super::tools::{all_tools, dispatch};
use super::types::{
    InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse, ServerCapabilities,
    ServerInfo, ToolResult,
};

// ── McpServer ─────────────────────────────────────────────────────────────────

pub struct McpServer {
    config: AppConfig,
    sessions: SharedSessionManager,
}

impl McpServer {
    pub fn new(config: AppConfig, sessions: SharedSessionManager) -> Self {
        Self { config, sessions }
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

        let tool_result: ToolResult =
            dispatch(tool_name, &args, &self.config, &self.sessions).await;

        encode(JsonRpcResponse::ok(id, serde_json::to_value(tool_result).unwrap_or(Value::Null)))
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn encode<T: serde::Serialize>(v: T) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&v).unwrap_or_else(|_| b"null".to_vec());
    bytes.push(b'\n');
    bytes
}
