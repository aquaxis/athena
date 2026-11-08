//! A minimal HTTP/1.1 client for the Core API (X-27).
//!
//! The CLI must actually *perform* its requests (`R-IMPL-03`'s observable face: a verb
//! that prints `planned: …` is the pre-implementation state, not the implemented one).
//! It nevertheless stays a thin client on purpose:
//!
//! - **No TLS, no connection pool, no redirect following.** The Core API binds loopback
//!   over plain HTTP (`ATH-OPS-004`, `ATH-CFG-006`), so `http://` is the only scheme that
//!   can occur. A client that cannot speak TLS cannot be pointed at a remote host by
//!   accident either.
//! - **One request per connection** (`Connection: close`), because that is all the API's
//!   surface needs and it keeps the response read unambiguous: everything up to EOF is
//!   the body.
//!
//! The boundary rules are untouched: this module speaks HTTP to the Core API and nothing
//! else — no `agent-cli`, no Core crate (`ATH-CLI-001`, `ATH-CLI-002`).

use crate::endpoint::{Auth, Endpoint};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

/// The deadline for one request. Long enough for any Core API call the CLI makes (the
/// slowest is a Task create that queues real work), short enough that a wedged API does
/// not hang an operator's terminal.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// One HTTP response, in the shape the CLI renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// The `Content-Type`, when the server sent one.
    pub content_type: Option<String>,
    /// The body, decoded as UTF-8 (lossy: a binary body is not something the CLI prints).
    pub body: String,
}

impl Response {
    /// Whether the status is a 2xx.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// The body parsed as JSON, when it parses.
    #[must_use]
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.body).ok()
    }

    /// A one-line description of the outcome, for the failure path.
    #[must_use]
    pub fn summary(&self) -> String {
        let error_code = self
            .json()
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("code"))
            .and_then(serde_json::Value::as_str)
            .map(|code| format!(" ({code})"))
            .unwrap_or_default();
        format!("HTTP {}{error_code}", self.status)
    }
}

/// Why a request did not produce a response.
#[derive(Debug, thiserror::Error)]
pub enum HttpFault {
    /// The connection could not be opened or the exchange failed mid-flight.
    #[error("request to {url} failed: {detail}")]
    Transport {
        /// The URL attempted.
        url: String,
        /// What went wrong.
        detail: String,
    },
    /// The address in the configuration is not `host:port` over plain HTTP.
    #[error("the configured address {0} is not a plain http://host:port endpoint")]
    BadAddress(String),
}

/// Perform one request against the Core API.
///
/// `body` is sent as `application/json` when present.
///
/// # Errors
/// [`HttpFault::Transport`] for any I/O fault, [`HttpFault::BadAddress`] when the resolved
/// base cannot be parsed as a plain-HTTP authority.
pub fn request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<Response, HttpFault> {
    let url = endpoint.url(path);
    let authority = endpoint
        .base
        .strip_prefix("http://")
        .ok_or_else(|| HttpFault::BadAddress(endpoint.base.clone()))?;
    let transport = |detail: String| HttpFault::Transport {
        url: url.clone(),
        detail,
    };

    let stream = TcpStream::connect(authority).map_err(|error| transport(error.to_string()))?;
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|error| transport(error.to_string()))?;
    stream
        .set_write_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|error| transport(error.to_string()))?;

    let mut head = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {authority}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         User-Agent: athena-cli\r\n"
    );
    if let Auth::Bearer(token) = &endpoint.auth {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if let Some(body) = body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    if let Some(body) = body {
        head.push_str(body);
    }

    let mut stream = stream;
    stream
        .write_all(head.as_bytes())
        .map_err(|error| transport(error.to_string()))?;
    stream
        .flush()
        .map_err(|error| transport(error.to_string()))?;

    // The response: a status line, headers until the blank line, then the body to EOF
    // (`Connection: close` makes EOF the framing).
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|error| transport(error.to_string()))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| transport(format!("malformed status line {status_line:?}")))?;

    let mut content_type = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| transport(error.to_string()))?;
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-type")
        {
            content_type = Some(value.trim().to_string());
        }
    }

    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        use std::io::Read as _;
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(transport(error.to_string())),
        }
    }

    Ok(Response {
        status,
        content_type,
        body: String::from_utf8_lossy(&raw).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    use crate::endpoint::Endpoint;

    /// A one-shot HTTP server: reads the request, answers with `reply`, and reports the
    /// request text back so the test can assert what the client sent.
    fn one_shot(reply: String) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let authority = listener.local_addr().expect("addr").to_string();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut request = String::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("read") == 0 {
                    break;
                }
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            if content_length > 0 {
                use std::io::Read as _;
                reader.read_exact(&mut body).expect("body");
                request.push_str(&String::from_utf8_lossy(&body));
            }
            let mut stream = stream;
            stream.write_all(reply.as_bytes()).expect("reply");
            request
        });
        (authority, handle)
    }

    fn endpoint(authority: &str, auth: Auth) -> Endpoint {
        Endpoint {
            base: format!("http://{authority}"),
            auth,
        }
    }

    #[test]
    fn a_get_sends_the_bearer_and_reads_the_body() {
        let (authority, server) = one_shot(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":\"ready\"}"
                .to_string(),
        );
        let endpoint = endpoint(&authority, Auth::Bearer("ath-admin-tok".into()));
        let response = request(&endpoint, "GET", "/api/v1/status", None).expect("request");
        let seen = server.join().expect("server");

        assert!(response.is_success());
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type.as_deref(), Some("application/json"));
        assert_eq!(
            response
                .json()
                .and_then(|v| v["status"].as_str().map(str::to_string)),
            Some("ready".to_string())
        );
        assert!(seen.starts_with("GET /api/v1/status HTTP/1.1"), "{seen}");
        assert!(
            seen.contains("Authorization: Bearer ath-admin-tok"),
            "the bearer rides the request: {seen}"
        );
    }

    #[test]
    fn a_post_carries_its_json_body_and_length() {
        let (authority, server) = one_shot(
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\n\r\n{\"id\":\"task-1\"}"
                .to_string(),
        );
        let endpoint = endpoint(&authority, Auth::None);
        let body = r#"{"instruction":"hello","workspace":"/tmp/p","auto_run":true}"#;
        let response = request(&endpoint, "POST", "/api/v1/tasks", Some(body)).expect("request");
        let seen = server.join().expect("server");

        assert_eq!(response.status, 201);
        assert!(seen.starts_with("POST /api/v1/tasks HTTP/1.1"), "{seen}");
        assert!(seen.contains("Content-Type: application/json"), "{seen}");
        assert!(
            seen.contains(&format!("Content-Length: {}", body.len())),
            "{seen}"
        );
        assert!(seen.ends_with(body), "the body rides after the blank line");
        assert!(
            !seen.contains("Authorization:"),
            "none-mode sends no bearer"
        );
    }

    #[test]
    fn an_error_status_is_not_a_success_and_names_its_code() {
        let (authority, _server) = one_shot(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\r\n\
             {\"error\":{\"code\":\"TASK_NOT_FOUND\"}}"
                .to_string(),
        );
        let endpoint = endpoint(&authority, Auth::None);
        let response = request(&endpoint, "GET", "/api/v1/tasks/task-x", None).expect("request");
        assert!(!response.is_success());
        assert_eq!(response.status, 404);
        assert_eq!(response.summary(), "HTTP 404 (TASK_NOT_FOUND)");
    }

    #[test]
    fn a_closed_port_is_a_transport_fault() {
        // Bind then drop, so the port is (almost certainly) free and refusing.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let authority = listener.local_addr().expect("addr").to_string();
        drop(listener);
        let endpoint = endpoint(&authority, Auth::None);
        let fault = request(&endpoint, "GET", "/api/v1/status", None).expect_err("no server");
        assert!(matches!(fault, HttpFault::Transport { .. }), "{fault}");
    }

    #[test]
    fn a_non_http_base_is_refused() {
        let endpoint = Endpoint {
            base: "https://127.0.0.1:8080".into(),
            auth: Auth::None,
        };
        let fault = request(&endpoint, "GET", "/api/v1/status", None).expect_err("not http");
        assert!(matches!(fault, HttpFault::BadAddress(_)), "{fault}");
    }
}
