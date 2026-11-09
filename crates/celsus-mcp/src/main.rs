//! Celsus MCP server over standard input and output: JSON-RPC 2.0, one message per line. Diagnostics go to standard
//! error, never to standard output.

use std::io::{BufRead, Write};

use celsus_mcp::handle;
use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.iter().filter_map(handle).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            Ok(msg) => handle(&msg),
            Err(e) => Some(
                json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } }),
            ),
        };
        if let Some(r) = reply
            && writeln!(stdout, "{r}")
                .and_then(|()| stdout.flush())
                .is_err()
        {
            break;
        }
    }
}
