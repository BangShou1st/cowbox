use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

pub const CANONICAL_PROTOCOL_VERSION: &str = "2025-06-18";
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
pub const UPSTREAM_PROTOCOL_VERSION: &str = "2024-11-05";

pub const CONTROL_TOOLS: &[&str] = &["cowbox_diff", "cowbox_rollback", "cowbox_commit"];

pub const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 900 * 1024;
pub const MAX_TOOLS: usize = 10_000;
pub const MAX_LIST_PAGES: usize = 128;
pub const MAX_UPSTREAM_STDERR: usize = 64 * 1024;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
pub const SERVER_NOT_INITIALIZED: i64 = -32002;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterMode {
    List,
    Call,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterRequest {
    pub mode: AdapterMode,
    pub workspace: PathBuf,
    pub upstream: Vec<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub arguments: Value,
    pub timeout_ms: u64,
    pub max_response_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterOutput {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AdapterFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterFailure {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_stderr: Option<String>,
}

impl AdapterOutput {
    pub fn tools(
        tools: Vec<Value>,
        protocol_version: impl Into<String>,
        server_info: Value,
    ) -> Self {
        Self {
            ok: true,
            tools: Some(tools),
            protocol_version: Some(protocol_version.into()),
            server_info: Some(server_info),
            response: None,
            error: None,
        }
    }

    pub fn call(response: Value) -> Self {
        Self {
            ok: true,
            tools: None,
            protocol_version: None,
            server_info: None,
            response: Some(response),
            error: None,
        }
    }

    pub fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            tools: None,
            protocol_version: None,
            server_info: None,
            response: None,
            error: Some(AdapterFailure {
                code: code.into(),
                message: message.into(),
                hint: None,
                upstream_stderr: None,
            }),
        }
    }
}

pub fn negotiate_protocol_version(requested: Option<&str>) -> String {
    requested
        .filter(|version| SUPPORTED_PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(CANONICAL_PROTOCOL_VERSION)
        .to_string()
}

pub fn response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

pub fn error_response(
    id: Value,
    code: i64,
    message: impl Into<String>,
    data: Option<Value>,
) -> Value {
    let mut error = json!({
        "code": code,
        "message": message.into(),
    });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    })
}

pub fn is_control_tool(name: &str) -> bool {
    CONTROL_TOOLS.contains(&name)
}

pub fn control_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "cowbox_diff",
            "description": "Show the net sandbox changes for a cowbox snapshot. This is read-only and never changes the host.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "step": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Snapshot step to inspect. Defaults to the current session head."
                    }
                },
                "additionalProperties": false
            },
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        }),
        json!({
            "name": "cowbox_rollback",
            "description": "Move the cowbox session head to an earlier step. This changes sandbox state only; the host workspace is untouched.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "step": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Existing snapshot step to make the current head."
                    }
                },
                "required": ["step"],
                "additionalProperties": false
            },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        }),
        json!({
            "name": "cowbox_commit",
            "description": "Preview committing a cowbox snapshot with the existing dry-run checks. This never writes the host; the user must run `cowbox commit --step <n>` in a terminal and approve it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "step": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Snapshot step to preview. Defaults to the current session head."
                    }
                },
                "additionalProperties": false
            },
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_negotiation_keeps_supported_versions() {
        for version in SUPPORTED_PROTOCOL_VERSIONS {
            assert_eq!(negotiate_protocol_version(Some(version)), *version);
        }
        assert_eq!(
            negotiate_protocol_version(Some("2099-01-01")),
            CANONICAL_PROTOCOL_VERSION
        );
        assert_eq!(negotiate_protocol_version(None), CANONICAL_PROTOCOL_VERSION);
    }

    #[test]
    fn control_tool_names_stay_inside_the_supported_surface() {
        let tools = control_tools();
        let names = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, CONTROL_TOOLS);
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
    }
}
