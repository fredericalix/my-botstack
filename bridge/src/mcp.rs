//! JSON-RPC over HTTP for the two MCP endpoints, `/grok/mcp` (Grok Bot) and
//! `/fleet/mcp` (fleet administration). Plain data in and out, so the HTTP
//! layer stays thin and the behaviour is tested without a server.

use serde_json::{Value, json};

pub const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

pub struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

/// A JSON-RPC message whose envelope is valid.
pub enum Message {
    /// Answer with `result` or `method_not_found`.
    Request { id: Value, method: String, params: Value },
    /// No id (notifications/initialized and the like): answer `accepted`.
    Notification,
}

/// The message in `body`, or the 400 to answer.
pub fn parse(body: &[u8]) -> Result<Message, Response> {
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        return Err(json_response(400, json!({"error": "invalid json"}), vec![]));
    };
    if message.is_array() {
        return Err(json_response(400, json!({"error": "batch requests are not supported"}), vec![]));
    }
    if !message.is_object() {
        return Err(json_response(400, json!({"error": "expected a json-rpc object"}), vec![]));
    }
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if id.is_null() {
        return Ok(Message::Notification);
    }
    let method = message["method"].as_str().unwrap_or("").to_string();
    Ok(Message::Request { id, method, params: message["params"].clone() })
}

pub fn initialize(params: &Value, server: &str) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or("");
    let version = if VERSIONS.contains(&asked) { asked } else { VERSIONS[1] };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": server, "version": env!("CARGO_PKG_VERSION")},
    })
}

pub fn result(id: Value, result: Value, session: &str) -> Response {
    json_response(200, json!({"jsonrpc": "2.0", "id": id, "result": result}), vec![("Mcp-Session-Id", session.to_string())])
}

pub fn method_not_found(id: Value, method: &str, session: &str) -> Response {
    json_response(
        200,
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Method not found: {method}")}}),
        vec![("Mcp-Session-Id", session.to_string())],
    )
}

pub fn accepted(session: &str) -> Response {
    Response { status: 202, headers: vec![("Mcp-Session-Id", session.to_string())], body: Vec::new() }
}

/// A `tools/call`: the tool's name and its arguments (an empty object when missing).
pub fn call(params: &Value) -> (&str, Value) {
    let name = params["name"].as_str().unwrap_or("");
    let arguments = if params["arguments"].is_object() { params["arguments"].clone() } else { json!({}) };
    (name, arguments)
}

pub fn tool_result(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

pub fn json_response(status: u16, payload: Value, mut headers: Vec<(&'static str, String)>) -> Response {
    headers.insert(0, ("Content-Type", "application/json".into()));
    Response { status, headers, body: payload.to_string().into_bytes() }
}

pub fn string_field(value: &Value, key: &str) -> String {
    match &value[key] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `Authorization: Bearer <token>`, compared in constant time. An empty token refuses everyone.
pub fn bearer_ok(authorization: Option<&str>, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let Some((scheme, value)) = authorization.unwrap_or("").split_once(' ') else {
        return false;
    };
    scheme.eq_ignore_ascii_case("bearer") && !value.is_empty() && constant_time_eq(value.as_bytes(), token.as_bytes())
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sorts_requests_notifications_and_bad_bodies() {
        for raw in [&b"42"[..], b"\"text\"", b"not json", b"[1]"] {
            assert_eq!(parse(raw).err().map(|r| r.status), Some(400));
        }
        assert!(matches!(parse(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), Ok(Message::Notification)));
        match parse(br#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#) {
            Ok(Message::Request { id, method, .. }) => assert_eq!((id, method.as_str()), (json!(7), "tools/list")),
            _ => panic!("a request"),
        }
    }

    #[test]
    fn bearer_needs_the_exact_token() {
        assert!(bearer_ok(Some("Bearer secret"), "secret"));
        assert!(bearer_ok(Some("bearer secret"), "secret"));
        assert!(!bearer_ok(Some("Bearer wrong"), "secret"));
        assert!(!bearer_ok(None, "secret"));
        assert!(!bearer_ok(Some("Bearer "), ""));
    }

    #[test]
    fn initialize_echoes_a_known_version() {
        assert_eq!(initialize(&json!({"protocolVersion": "2025-06-18"}), "x")["protocolVersion"], "2025-06-18");
        assert_eq!(initialize(&json!({"protocolVersion": "1999-01-01"}), "x")["protocolVersion"], "2025-03-26");
        assert_eq!(initialize(&Value::Null, "botstack-fleet")["serverInfo"]["name"], "botstack-fleet");
    }

    #[test]
    fn call_reads_name_and_arguments() {
        let params = json!({"name": "fleet_list", "arguments": {"device": "cuisine"}});
        assert_eq!(call(&params), ("fleet_list", json!({"device": "cuisine"})));
        assert_eq!(call(&json!("x")), ("", json!({})));
    }
}
