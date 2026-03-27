use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── JSON-RPC 2.0 ─────────────────────────────────────────────────────────────

/// Inbound JSON-RPC message (request or notification).
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// `None` for notifications, `Some` for requests.
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// Returns `true` if this is a notification (no `id` field).
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// Outbound JSON-RPC success response.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub result: Value,
}

impl JsonRpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result,
        }
    }
}

/// Outbound JSON-RPC error response.
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub error: RpcErrorBody,
}

impl JsonRpcError {
    pub fn new(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            error: RpcErrorBody {
                code,
                message: message.into(),
            },
        }
    }

    pub fn method_not_found(id: Value, method: &str) -> Self {
        Self::new(id, -32601, format!("Method not found: {method}"))
    }

    pub fn invalid_params(id: Value, reason: impl Into<String>) -> Self {
        Self::new(id, -32602, format!("Invalid params: {}", reason.into()))
    }

    pub fn internal(id: Value, reason: impl Into<String>) -> Self {
        Self::new(id, -32603, format!("Internal error: {}", reason.into()))
    }
}

#[derive(Debug, Serialize)]
pub struct RpcErrorBody {
    pub code: i32,
    pub message: String,
}

// ── MCP protocol objects ──────────────────────────────────────────────────────

/// MCP `initialize` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: &'static str,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
}

#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    pub tools: Value,
}

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}

/// One entry in `tools/list` response.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// MCP tool-call response (a list of content items).
#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub content: Vec<ContentItem>,
    /// Set to `true` when the tool reports an error inside a successful call.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: vec![ContentItem::text(s)],
            is_error: false,
        }
    }

    pub fn error(s: impl Into<String>) -> Self {
        Self {
            content: vec![ContentItem::text(s)],
            is_error: true,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ContentItem {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
}

impl ContentItem {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            kind: "text",
            text: s.into(),
        }
    }
}
