//! The JSON-RPC 2.0 envelope, hand-rolled.
//!
//! MCP's stdio transport is newline-delimited JSON-RPC and nothing else: one
//! object per line, no framing header, and no embedded newlines — which
//! `serde_json::to_string` gives us for free, since it escapes every newline
//! inside a string. That is the entire framing problem, and it is why this
//! module is eighty lines instead of a dependency.
//!
//! The rule everything here serves: **a request that arrives is answered
//! exactly once, and a notification is never answered.** A response to a
//! notification is a protocol violation that clients report as a corrupt
//! stream, which is a much worse failure than the malformed input that
//! provoked it.

use serde_json::{json, Value};

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

/// One line off the wire, classified by what it obliges us to do.
pub enum Incoming {
    /// A response is owed, carrying this `id` verbatim.
    Call {
        id: Value,
        method: String,
        params: Value,
    },
    /// A notification: no response, ever, including for a method we do not
    /// know.
    ///
    /// The method and params are discarded here rather than carried, because
    /// nothing this server could receive as a notification changes anything.
    /// The only one Claude Code sends is `notifications/initialized`, and tool
    /// calls are deliberately *not* gated on having seen it: refusing them
    /// before it arrives would convert a client's ordering into a mysterious
    /// permission failure, and the ordering is the client's business.
    Notify,
    /// Framing noise. Answering a blank line with a parse error would put a
    /// line on the wire that nothing asked for.
    Blank,
    /// Unparseable or not a request. `id` is `None` when the line was too
    /// broken to recover one, which is the case the spec answers with a null
    /// id.
    Malformed {
        id: Option<Value>,
        code: i64,
        message: String,
    },
}

pub fn parse(line: &str) -> Incoming {
    let line = line.trim();
    if line.is_empty() {
        return Incoming::Blank;
    }

    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Incoming::Malformed {
                id: None,
                code: PARSE_ERROR,
                message: format!("invalid JSON: {e}"),
            }
        }
    };

    let Value::Object(mut obj) = value else {
        // A JSON-RPC batch is a top-level array. MCP required batching in
        // 2025-03-26 and *removed* it in 2025-06-18, and Claude Code has never
        // sent one; refusing it is both correct and the honest description of
        // what this server does.
        let message = if value.is_array() {
            "JSON-RPC batches are not supported"
        } else {
            "a request must be a JSON object"
        };
        return Incoming::Malformed {
            id: None,
            code: INVALID_REQUEST,
            message: message.to_owned(),
        };
    };

    // A null id is treated as absent. The spec says a notification omits the
    // key; a client that spells "no id" as null means the same thing, and
    // answering it would break the rule above.
    let id = match obj.remove("id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v),
    };

    // Only a version we can positively identify as wrong is refused. A missing
    // or oddly typed `jsonrpc` is not worth failing a request that names a
    // method we implement.
    if let Some(v) = obj.get("jsonrpc").and_then(Value::as_str) {
        if v != "2.0" {
            return Incoming::Malformed {
                id,
                code: INVALID_REQUEST,
                message: format!("unsupported jsonrpc version {v:?}"),
            };
        }
    }

    let Some(method) = obj.get("method").and_then(Value::as_str).map(str::to_owned) else {
        return Incoming::Malformed {
            id,
            code: INVALID_REQUEST,
            message: "missing or non-string `method`".to_owned(),
        };
    };
    let params = obj.remove("params").unwrap_or(Value::Null);

    match id {
        Some(id) => Incoming::Call { id, method, params },
        None => Incoming::Notify,
    }
}

pub fn result(id: &Value, result: Value) -> String {
    line(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

pub fn error(id: Option<&Value>, code: i64, message: &str) -> String {
    line(json!({
        "jsonrpc": "2.0",
        "id": id.cloned().unwrap_or(Value::Null),
        "error": { "code": code, "message": message },
    }))
}

fn line(v: Value) -> String {
    // Total, not optimistic: the only ways `to_string` fails are a non-string
    // map key and a non-finite float, and `serde_json::Value` can represent
    // neither. `Server::handle_line`'s promise never to return a malformed
    // line rests on exactly that.
    serde_json::to_string(&v).expect("a serde_json::Value always serializes")
}
