//! JSON-RPC 2.0 and MCP protocol constants and helpers.

use serde_json::{Value, json};

/// Protocol revisions this server understands, newest first.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The newest supported protocol revision.
pub const LATEST_PROTOCOL_VERSION: &str = SUPPORTED_PROTOCOL_VERSIONS[0];

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// MCP: requested resource does not exist.
pub const RESOURCE_NOT_FOUND: i64 = -32002;

/// Picks the protocol version to answer an `initialize` request with.
///
/// The client's version is echoed when supported, otherwise the newest
/// version this server implements is returned.
pub fn negotiate_version(requested: Option<&str>) -> &'static str {
    requested.and_then(|r| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|v| **v == r)).copied().unwrap_or(LATEST_PROTOCOL_VERSION)
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), data: None }
    }
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }
    pub fn method_not_found(method: &str) -> Self {
        Self::new(METHOD_NOT_FOUND, format!("Method not found: {method}"))
    }
    pub fn to_json(&self) -> Value {
        let mut e = json!({ "code": self.code, "message": self.message });
        if let Some(d) = &self.data {
            crate::args::set(&mut e, "data", d.clone());
        }
        e
    }
}

/// Builds a success response.
pub fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Builds an error response.
pub fn error_response(id: Value, err: &RpcError) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": err.to_json() })
}

/// Error response for a message that could not be parsed as JSON.
pub fn parse_error(detail: impl std::fmt::Display) -> Value {
    error_response(Value::Null, &RpcError::new(PARSE_ERROR, format!("Parse error: {detail}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_negotiation() {
        assert_eq!(negotiate_version(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate_version(Some("2099-01-01")), LATEST_PROTOCOL_VERSION);
        assert_eq!(negotiate_version(None), LATEST_PROTOCOL_VERSION);
    }
}
