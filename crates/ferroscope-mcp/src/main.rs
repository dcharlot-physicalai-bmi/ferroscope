//! The Ferroscope MCP server.
//!
//! Speaks JSON-RPC 2.0 over stdio, one message per line, so an agent can say what scene it wants
//! and get back a recording with a receipt on it. The tools are in [`tools`]; this file is the
//! transport and nothing else.
//!
//! ```text
//! $ ferroscope-mcp                      # then write JSON-RPC on stdin
//! {"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
//! {"jsonrpc":"2.0","id":2,"method":"tools/list"}
//! {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"scene_schema"}}
//! ```
//!
//! Register it with a client by pointing at the binary; it needs no configuration, no network
//! and no account, and it writes only where the caller tells it to.

#![forbid(unsafe_code)]

mod tools;

use ferroscope_schema::json::{self, Value};
use std::io::{BufRead, Write};

/// The protocol revision this server implements.
/// The MCP revisions this server will speak, newest first.
///
/// It answers `initialize` by ECHOING the client's requested version when it is one of these,
/// and falling back to the newest otherwise — which is what the lifecycle asks for: a server
/// that supports the requested version must reply with that version, and a client offered one
/// it does not know should disconnect. This server replied `2025-06-18` to every request
/// regardless, so a client that speaks only an earlier revision was told to go away over a
/// difference that does not exist here. Nothing in this tool surface — `tools/list` and
/// `tools/call` — changed across these three.
const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// The newest revision this server speaks, used when the client asks for nothing or for
/// something unrecognised.
const PROTOCOL: &str = PROTOCOLS[0];

/// The version to answer `initialize` with, given what the client asked for.
fn negotiated(asked: Option<&str>) -> &'static str {
    match asked {
        Some(v) => PROTOCOLS.into_iter().find(|p| *p == v).unwrap_or(PROTOCOL),
        None => PROTOCOL,
    }
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A message that cannot be parsed still gets an answer, because a client waiting on an
        // id it will never see is worse than an error it can read.
        let Some(msg) = json::parse(line) else {
            let _ = writeln!(stdout, "{}", error(&Value::Null, -32700, "invalid JSON"));
            let _ = stdout.flush();
            continue;
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // Notifications carry no id and take no reply, by the specification. Answering one is
        // a protocol violation that some clients treat as a fatal desync.
        let is_notification = msg.get("id").is_none();

        let out = match method {
            "initialize" => Some(result(
                &id,
                &format!(
                    r#"{{"protocolVersion":{},"capabilities":{{"tools":{{"listChanged":false}}}},"serverInfo":{{"name":"ferroscope","version":{}}}}}"#,
                    quote(negotiated(
                        params.get("protocolVersion").and_then(|v| v.as_str())
                    )),
                    quote(env!("CARGO_PKG_VERSION"))
                ),
            )),
            "tools/list" => Some(result(&id, &format!(r#"{{"tools":[{}]}}"#, tools::list()))),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                Some(result(&id, &tools::call(name, &args)))
            }
            "ping" => Some(result(&id, "{}")),
            _ if is_notification => None,
            _ => Some(error(&id, -32601, &format!("no such method: {method:?}"))),
        };
        if let Some(out) = out {
            let _ = writeln!(stdout, "{out}");
            let _ = stdout.flush();
        }
    }
}

fn quote(s: &str) -> String {
    let mut out = String::new();
    json::write_string(&mut out, s);
    out
}

fn result(id: &Value, raw: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"result":{raw}}}"#,
        id.to_json()
    )
}

fn error(id: &Value, code: i32, message: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":{code},"message":{}}}}}"#,
        id.to_json(),
        quote(message)
    )
}
