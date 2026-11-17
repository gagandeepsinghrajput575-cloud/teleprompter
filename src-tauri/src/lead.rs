//! Loopback intake for the dialer.
//!
//! Pulse — the dialer that runs alongside this app — POSTs the business owner's name here the
//! moment it connects a call, so the script's placeholders are already filled when the line is
//! answered. Nothing leaves the machine and no other route is exposed.
//!
//! Deliberate limits, because a loopback port is reachable by *every* process on the box and,
//! via `fetch()` to `127.0.0.1`, by any web page the user happens to have open:
//!
//!  * `127.0.0.1` only, one route (`POST /lead`), body capped at 4 KB;
//!  * any request carrying `Origin` or `Referer` is refused — browsers always send one on a
//!    cross-origin fetch, a native dialer sends neither, so this is what keeps a stray web page
//!    from writing into the HUD;
//!  * an optional shared token (`STEALTH_LEAD_TOKEN`) for a dialer that can send a header;
//!  * socket read/write timeouts, so one half-open connection cannot pin the accept loop;
//!  * the name is sanitised before it is ever rendered: one line, bounded, no control characters.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// Fixed so the dialer only needs configuring once. Override with `STEALTH_LEAD_PORT`.
pub const DEFAULT_PORT: u16 = 47_831;
const MAX_BODY: usize = 4 * 1024;
const MAX_NAME_CHARS: usize = 48;
const IO_TIMEOUT: Duration = Duration::from_secs(3);

/// What the dialer sends. The name is the only thing the script needs; the aliases exist so a
/// dialer that calls it `owner_name` or `firstName` still works without being reconfigured.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Lead {
    #[serde(
        default,
        alias = "owner",
        alias = "owner_name",
        alias = "ownerName",
        alias = "prospect",
        alias = "prospect_name",
        alias = "prospectName",
        alias = "first_name",
        alias = "firstName"
    )]
    pub name: Option<String>,
    #[serde(
        default,
        alias = "company_name",
        alias = "companyName",
        alias = "business",
        alias = "business_name"
    )]
    pub company: Option<String>,
    #[serde(
        default,
        alias = "industry",
        alias = "trade"
    )]
    pub niche: Option<String>,
    #[serde(
        default,
        alias = "employees",
        alias = "no_of_employees",
        alias = "no_of_staff",
        alias = "team_size",
        alias = "teamSize",
        alias = "crew_size",
        alias = "crewSize",
        alias = "staff_count",
        alias = "staffCount",
        alias = "num_employees",
        alias = "numberOfEmployees",
        alias = "employee_count",
        alias = "employeeCount",
        alias = "headcount"
    )]
    pub employees: Option<String>,
}

/// Cleans a name for display.
///
/// This string is rendered *inside* the script, so control characters are dropped and whitespace
/// is collapsed — a newline would break the card layout — and the length is bounded so an
/// oversized value cannot push the copy off screen.
pub fn sanitize(raw: &str) -> String {
    let flattened: String = raw
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    let joined = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(MAX_NAME_CHARS).collect()
}

/// Starts the intake on `port` (`0` picks a free one) and returns the port actually bound.
///
/// `on_lead` runs on the listener thread, so it must be cheap: the app only stores the name and
/// emits an event.
pub fn spawn<F>(port: u16, token: Option<String>, on_lead: F) -> Result<u16, String>
where
    F: Fn(Lead) + Send + 'static,
{
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("cannot bind 127.0.0.1:{port}: {e}"))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("cannot read bound address: {e}"))?
        .port();

    std::thread::Builder::new()
        .name("lead-intake".into())
        .spawn(move || {
            println!("LEAD_LISTENER bound=127.0.0.1:{bound} route='POST /lead'");
            // Handled inline rather than thread-per-connection: a dialer sends one tiny POST per
            // call, and a sequential loop cannot be exhausted by a flood of connections.
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => match handle(stream, token.as_deref()) {
                        Ok(Some(lead)) => on_lead(lead),
                        Ok(None) => {}
                        Err(err) => eprintln!("LEAD_ERROR {err}"),
                    },
                    Err(err) => eprintln!("LEAD_ACCEPT_ERROR {err}"),
                }
            }
        })
        .map_err(|e| format!("cannot spawn lead intake thread: {e}"))?;

    Ok(bound)
}

fn handle(mut stream: TcpStream, token: Option<&str>) -> Result<Option<Lead>, String> {
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|e| format!("read timeout: {e}"))?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|e| format!("write timeout: {e}"))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| format!("clone: {e}"))?);

    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| format!("request line: {e}"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut length = 0usize;
    let mut from_browser = false;
    let mut presented: Option<String> = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| format!("headers: {e}"))?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().unwrap_or(0),
            "origin" | "referer" => from_browser = true,
            "x-stealth-token" => presented = Some(value.trim().to_string()),
            _ => {}
        }
    }

    if method != "POST" || path != "/lead" {
        respond(&mut stream, 404, r#"{"error":"POST /lead only"}"#);
        return Ok(None);
    }
    if from_browser {
        respond(&mut stream, 403, r#"{"error":"browser origins are refused"}"#);
        return Ok(None);
    }
    if let Some(expected) = token {
        if presented.as_deref() != Some(expected) {
            respond(&mut stream, 401, r#"{"error":"missing or wrong X-Stealth-Token"}"#);
            return Ok(None);
        }
    }
    if length > MAX_BODY {
        respond(&mut stream, 413, r#"{"error":"body too large"}"#);
        return Ok(None);
    }
    if length == 0 {
        respond(&mut stream, 400, r#"{"error":"empty body"}"#);
        return Ok(None);
    }

    let mut body = vec![0u8; length];
    if let Err(err) = reader.read_exact(&mut body) {
        respond(&mut stream, 400, r#"{"error":"truncated body"}"#);
        return Err(format!("body read: {err}"));
    }

    let lead: Lead = match serde_json::from_slice(&body) {
        Ok(lead) => lead,
        Err(err) => {
            respond(&mut stream, 400, r#"{"error":"body is not JSON"}"#);
            return Err(format!("invalid lead json: {err}"));
        }
    };

    let name = lead.name.as_deref().map(sanitize).unwrap_or_default();
    if name.is_empty() {
        respond(&mut stream, 400, r#"{"error":"name is required"}"#);
        return Ok(None);
    }
    let company = lead.company.as_deref().map(sanitize);
    let niche = lead.niche.as_deref().map(sanitize);
    let employees = lead.employees.as_deref().map(sanitize);

    respond(&mut stream, 200, r#"{"ok":true}"#);
    Ok(Some(Lead {
        name: Some(name),
        company,
        niche,
        employees,
    }))
}

fn respond(stream: &mut TcpStream, code: u16, body: &str) {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\ncontent-type: application/json\r\n\
content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    // A dialer that already hung up is not worth an error: the lead was recorded either way, and
    // the next call opens a fresh connection.
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::sync::mpsc;

    fn post(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream.write_all(request.as_bytes()).expect("write");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    }

    fn request(body: &str) -> String {
        format!(
            "POST /lead HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\n\
content-length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn sanitize_flattens_and_bounds() {
        assert_eq!(sanitize("  Mike\n\tJensen  "), "Mike Jensen");
        assert_eq!(sanitize("a\u{0}b"), "ab");
        assert_eq!(sanitize(&"x".repeat(200)).chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn accepts_the_dialer_aliases() {
        for raw in [
            r#"{"name":"Mike"}"#,
            r#"{"owner_name":"Mike"}"#,
            r#"{"firstName":"Mike"}"#,
        ] {
            let lead: Lead = serde_json::from_str(raw).expect(raw);
            assert_eq!(lead.name.as_deref(), Some("Mike"), "{raw}");
        }
    }

    #[test]
    fn a_posted_lead_is_delivered() {
        let (tx, rx) = mpsc::channel();
        let port = spawn(0, None, move |lead| {
            let _ = tx.send(lead);
        })
        .expect("spawn");
        let response = post(port, &request(r#"{"name":"Mike Jensen"}"#));
        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        let lead = rx.recv_timeout(Duration::from_secs(2)).expect("delivered");
        assert_eq!(lead.name.as_deref(), Some("Mike Jensen"));
    }

    /// Regression: a page could otherwise POST to the loopback port and rewrite what the rep
    /// reads mid-call.
    #[test]
    fn a_browser_origin_is_refused() {
        let (tx, rx) = mpsc::channel();
        let port = spawn(0, None, move |lead| {
            let _ = tx.send(lead);
        })
        .expect("spawn");
        let hostile = request(r#"{"name":"Mallory"}"#).replace(
            "content-type",
            "origin: https://evil.example\r\ncontent-type",
        );
        let response = post(port, &hostile);
        assert!(response.starts_with("HTTP/1.1 403"), "got: {response}");
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "a browser request must never be delivered"
        );
    }

    #[test]
    fn the_token_is_enforced_when_set() {
        let (tx, rx) = mpsc::channel();
        let port = spawn(0, Some("s3cret".to_string()), move |lead| {
            let _ = tx.send(lead);
        })
        .expect("spawn");

        let response = post(port, &request(r#"{"name":"Mike"}"#));
        assert!(response.starts_with("HTTP/1.1 401"), "got: {response}");

        let with_token = request(r#"{"name":"Mike"}"#).replace(
            "content-type",
            "x-stealth-token: s3cret\r\ncontent-type",
        );
        let response = post(port, &with_token);
        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2))
                .expect("delivered")
                .name
                .as_deref(),
            Some("Mike")
        );
    }
}
