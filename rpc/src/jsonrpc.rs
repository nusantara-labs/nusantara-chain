// JSON-RPC 2.0 request/response types for Nusantara blockchain.
//
// This module implements the JSON-RPC 2.0 specification (https://www.jsonrpc.org/specification)
// to provide Solana-compatible RPC access alongside the existing REST API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 standard error codes.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

/// A JSON-RPC 2.0 request object.
///
/// The `id` field is optional for notifications (which the server does not
/// respond to), but required for standard requests.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
    pub id: Option<Value>,
}

/// A JSON-RPC 2.0 response object.
///
/// Exactly one of `result` or `error` will be present in a well-formed response.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Value,
}

/// A JSON-RPC 2.0 error object embedded in the response.
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    /// Build a success response with the given result value.
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Build an error response with the given code and message.
    pub fn error(id: Value, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0",
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
            id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_request_with_params() {
        let json = r#"{"jsonrpc":"2.0","method":"getSlot","params":[],"id":1}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.method, "getSlot");
        assert!(req.params.is_some());
        assert_eq!(req.id, Some(Value::Number(1.into())));
    }

    #[test]
    fn deserialize_request_without_params() {
        let json = r#"{"jsonrpc":"2.0","method":"getHealth","id":"abc"}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "getHealth");
        assert!(req.params.is_none());
        assert_eq!(req.id, Some(Value::String("abc".to_string())));
    }

    #[test]
    fn deserialize_notification_no_id() {
        let json = r#"{"jsonrpc":"2.0","method":"notify","params":[1]}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert!(req.id.is_none());
    }

    #[test]
    fn serialize_success_response() {
        let resp = JsonRpcResponse::success(Value::Number(1.into()), serde_json::json!("ok"));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""jsonrpc":"2.0""#));
        assert!(json.contains(r#""result":"ok""#));
        assert!(!json.contains(r#""error""#));
    }

    #[test]
    fn serialize_error_response() {
        let resp = JsonRpcResponse::error(Value::Null, METHOD_NOT_FOUND, "not found".to_string());
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""error""#));
        assert!(json.contains(r#""code":-32601"#));
        assert!(!json.contains(r#""result""#));
    }

    #[test]
    fn error_response_omits_null_data() {
        let resp = JsonRpcResponse::error(Value::Null, INTERNAL_ERROR, "oops".to_string());
        let json = serde_json::to_string(&resp).unwrap();
        // The `data` field should not appear when it is None
        assert!(!json.contains(r#""data""#));
    }
}
