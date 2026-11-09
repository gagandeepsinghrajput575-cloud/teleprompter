//! Loopback webhook that lets a local dialer push the prospect's context (name, company, niche)
//! straight into the HUD when a call starts.
//!
//! Bound to `127.0.0.1` only. A malformed request can never take the app down, and a port that is
//! unavailable is logged and ignored — the HUD keeps working without it.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use tauri::Emitter;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CallContext {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub company: String,
    #[serde(default)]
    pub niche: String,
    #[serde(default)]
    pub script: Option<String>,
}

static CONTEXT: Mutex<Option<CallContext>> = Mutex::new(None);

/// The most recent context the dialer pushed, so a UI that subscribes late can still fetch it.
pub fn current() -> Option<CallContext> {
    CONTEXT.lock().ok().and_then(|slot| slot.clone())
}

/// Parses the JSON body of a `/context` request. Pure so it can be unit-tested without sockets.
pub fn parse_context(body: &[u8]) -> Result<CallContext, String> {
    serde_json::from_slice(body).map_err(|e| format!("invalid json: {e}"))
}

fn find_double_crlf(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Not Found",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Reads one small request (headers + Content-Length body).
/// Returns `(request_line, header_block, body)`.
fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let mut data: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(pos) = find_double_crlf(&data) {
            let headers = String::from_utf8_lossy(&data[..pos]).to_string();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            // A local client cannot legitimately send a huge body; refuse rather than allocate.
            if content_length > 1_000_000 {
                return None;
            }
            let body_start = pos + 4;
            while data.len() < body_start + content_length {
                let n = stream.read(&mut buf).ok()?;
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
            }
            let end = (body_start + content_length).min(data.len());
            let body = String::from_utf8_lossy(&data[body_start..end]).to_string();
            let request_line = headers.lines().next().unwrap_or("").to_string();
            return Some((request_line, headers, body));
        }
        if data.len() > 65_536 {
            break;
        }
    }
    None
}

fn handle(mut stream: TcpStream, app: &tauri::AppHandle) {
    let Some((request_line, headers, body)) = read_request(&mut stream) else {
        respond(&mut stream, 400, "{\"ok\":false}");
        return;
    };
    // A browser sends an Origin header on cross-site requests; the local dialer (Node) and curl do
    // not. Refuse browser-originated requests so a random web page cannot set the call context.
    if headers.to_ascii_lowercase().contains("\norigin:") {
        println!("CALL_CONTEXT_REJECTED browser origin");
        respond(
            &mut stream,
            403,
            "{\"ok\":false,\"error\":\"origin not allowed\"}",
        );
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    if method == "GET" && path == "/health" {
        respond(&mut stream, 200, "{\"ok\":true}");
        return;
    }

    if method == "POST" && (path == "/context" || path == "/call/start") {
        match parse_context(body.as_bytes()) {
            Ok(ctx) => {
                if let Ok(mut slot) = CONTEXT.lock() {
                    *slot = Some(ctx.clone());
                }
                let _ = app.emit("call_context", &ctx);
                println!(
                    "CALL_CONTEXT name='{}' company='{}' niche='{}' script={:?}",
                    ctx.name, ctx.company, ctx.niche, ctx.script
                );
                respond(&mut stream, 200, "{\"ok\":true}");
            }
            Err(err) => {
                println!("CALL_CONTEXT_BAD {err}");
                respond(&mut stream, 400, "{\"ok\":false}");
            }
        }
        return;
    }

    respond(&mut stream, 404, "{\"ok\":false}");
}

/// Starts the loopback listener on `127.0.0.1:$TELEPROMPTER_PORT` (default 8765). Non-fatal:
/// if the port cannot be bound, the HUD simply runs without dialer context.
pub fn spawn(app: tauri::AppHandle) {
    let port: u16 = std::env::var("TELEPROMPTER_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8765);
    std::thread::Builder::new()
        .name("teleprompter-context-http".into())
        .spawn(move || {
            let addr = format!("127.0.0.1:{port}");
            let listener = match TcpListener::bind(&addr) {
                Ok(listener) => listener,
                Err(err) => {
                    eprintln!("CALL_CONTEXT_LISTEN_FAILED {addr}: {err}");
                    return;
                }
            };
            println!("CALL_CONTEXT_LISTENING http://{addr}/context");
            for stream in listener.incoming().flatten() {
                // One thread per connection: a hung/slow client can never block a real push.
                let app = app.clone();
                std::thread::spawn(move || handle(stream, &app));
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::parse_context;

    #[test]
    fn parses_a_full_context_payload() {
        let ctx =
            parse_context(br#"{"name":"John","company":"Ridgeline","niche":"roofing"}"#).unwrap();
        assert_eq!(ctx.name, "John");
        assert_eq!(ctx.company, "Ridgeline");
        assert_eq!(ctx.niche, "roofing");
        assert!(ctx.script.is_none());
    }

    #[test]
    fn missing_fields_default_to_empty() {
        let ctx = parse_context(br#"{"name":"John"}"#).unwrap();
        assert_eq!(ctx.name, "John");
        assert_eq!(ctx.company, "");
        assert_eq!(ctx.niche, "");
    }

    #[test]
    fn rejects_malformed_json_without_panicking() {
        assert!(parse_context(b"{not json}").is_err());
    }
}
