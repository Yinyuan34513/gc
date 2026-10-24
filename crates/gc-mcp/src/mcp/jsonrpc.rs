//! JSON-RPC 2.0 报文类型（MCP stdio 传输用换行分隔，不是 LSP 的 Content-Length 分帧）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// JSON-RPC 标准错误码。
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
/// MCP 现代 era 专用错误码：客户端请求的协议版本服务端不支持。
/// 服务端**必须**在 `data` 里给出 `supported` / `requested`（见 spec 2026-07-28）。
pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;

/// 一条入站报文。`id` 缺失表示这是通知（notification），按规范**不能**回复。
#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// 一条出站报文。`result` 与 `error` 互斥。
#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ErrorObject {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// 带 `data` 字段的错误响应（用于 `UnsupportedProtocolVersionError` 等需要
    /// 在 `data` 里附带 `supported` / `requested` 的场景）。
    pub fn err_with_data(id: Value, code: i32, message: impl Into<String>, data: Value) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ErrorObject {
                code,
                message: message.into(),
                data: Some(data),
            }),
        }
    }

    /// 序列化成单行 JSON。stdio 传输要求报文内不含换行。
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            json!({
                "jsonrpc": "2.0",
                "id": self.id,
                "error": { "code": -32603, "message": format!("序列化响应失败: {e}") }
            })
            .to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_and_notification() {
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        assert!(!r.is_notification());
        assert_eq!(r.method, "ping");

        let n: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(n.is_notification());
    }

    #[test]
    fn missing_params_defaults_to_null() {
        let r: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"x"}"#).unwrap();
        assert!(r.params.is_null());
    }

    #[test]
    fn string_ids_are_preserved() {
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#).unwrap();
        let resp = Response::ok(r.id.unwrap(), json!({}));
        assert!(resp.to_line().contains(r#""id":"abc""#));
    }

    #[test]
    fn response_line_has_no_newlines() {
        let resp = Response::ok(json!(1), json!({"text": "a\nb"}));
        let line = resp.to_line();
        assert!(!line.contains('\n'), "报文内不得含裸换行: {line}");
    }

    #[test]
    fn error_omits_result_field() {
        let line = Response::err(json!(7), METHOD_NOT_FOUND, "nope").to_line();
        assert!(!line.contains("\"result\""));
        assert!(line.contains("-32601"));
    }
}
