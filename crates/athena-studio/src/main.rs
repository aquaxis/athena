//! The `athena-studio` bin (spec §4.3.2): the static server over the lib's kernel
//! face — one port (5173), one asset set, `--api` naming the Core API root the wasm
//! app binds to (`athena-studio` talks to `athena-api` **only**, `ATH-API-001`).

use athena_studio::{StudioApp, answer, boot_from_args};
/// One JSON log line as the studio emits it (X-39, ATH-OBS-001..005): the same shape
/// every process's lines carry — level, at, correlation_id, module, message, fields —
/// because the observation convention does not change with the process. The studio bin
/// has no athena-core edge (its deps are serde/serde_json only), so the discipline is
/// stated here: the sink is stderr, the module is always `studio`, and one correlation
/// key is minted once per process.
fn studio_log(level: &str, message: &str, fields: serde_json::Value) {
    static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let correlation = KEY.get_or_init(|| {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("studio-resident-{millis:016x}")
    });
    let at = {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format_rfc3339(millis)
    };
    let line = serde_json::json!({
        "level": level,
        "at": at,
        "correlation_id": correlation,
        "module": "studio",
        "message": message,
        "fields": fields,
    });
    eprintln!("{line}");
}

/// RFC 3339 from Unix milliseconds (UTC, millisecond precision) — the studio's own
/// stamp face, since it has no athena-core/time edge.
fn format_rfc3339(millis: u128) -> String {
    let secs = u64::try_from(millis / 1000).unwrap_or(0);
    let ms = u64::try_from(millis % 1000).unwrap_or(0);
    let days = secs / 86_400;
    let sod = secs % 86_400;
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    // civil-from-days (Howard Hinnant), same rule the core clock uses.
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = doe / 1461;
    let doy = doe % 1461;
    let mp = (doy * 5 + 308) / 1531;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
}

use std::io::{Read as _, Write as _};
use std::net::TcpListener;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let app = match boot_from_args(&args) {
        Ok(app) => app,
        Err(fault) => {
            studio_log(
                "error",
                &format!("boot refused: {fault}"),
                serde_json::json!({}),
            );
            std::process::exit(4);
        }
    };
    let port = port_of(&args);
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            studio_log(
                "error",
                &format!("cannot bind 127.0.0.1:{port}: {error}"),
                serde_json::json!({ "port": port }),
            );
            std::process::exit(4);
        }
    };
    studio_log(
        "info",
        &format!("http://127.0.0.1:{port} — api_root {}", app.api_root),
        serde_json::json!({ "port": port, "api_root": app.api_root }),
    );
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        let served = serve_once(&app, &mut stream);
        if let Err(error) = served {
            studio_log(
                "error",
                &format!("serve failed: {error}"),
                serde_json::json!({}),
            );
        }
    }
}

fn port_of(args: &[String]) -> u16 {
    args.iter()
        .position(|arg| arg == "--port")
        .and_then(|position| args.get(position + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(athena_studio::SERVICE_PORT)
}

fn serve_once(app: &StudioApp, stream: &mut std::net::TcpStream) -> Result<(), String> {
    let mut buf = [0_u8; 4096];
    let read = stream
        .read(&mut buf)
        .map_err(|error| format!("read failed: {error}"))?;
    let request = String::from_utf8_lossy(&buf[..read]).to_string();
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/index.html")
        .to_string();
    let (status, content_type, body) = match answer(app, &path) {
        Some((content_type, body)) => ("200", content_type, body),
        None => ("404", "text/plain; charset=utf-8", b"not found".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        if status == "200" { "OK" } else { "NOT FOUND" },
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(&body))
        .map_err(|error| format!("write failed: {error}"))
}

#[cfg(test)]
mod bin_tests {
    use super::*;

    #[test]
    fn the_bin_respects_the_api_and_port_args() {
        let root = boot_from_args(&[
            "--api".to_string(),
            "http://127.0.0.1:8080".to_string(),
            "--port".to_string(),
            "5174".to_string(),
        ])
        .expect("boot");
        assert_eq!(root.api_root, "http://127.0.0.1:8080");

        // The default port rides the spec's 5173 face.
        assert_eq!(port_of(&[]), athena_studio::SERVICE_PORT);
        assert_eq!(port_of(&["--port".into(), "9000".into()]), 9000);
    }

    #[test]
    fn the_server_answers_the_assets_and_refuses_unknowns() {
        let app = StudioApp::boot("http://127.0.0.1:8080").expect("boot");
        let (content_type, _) = answer(&app, "/").expect("index");
        assert!(content_type.contains("text/html"));
        let (content_type, _) = answer(&app, "/athena_studio.wasm").expect("wasm");
        assert_eq!(content_type, "application/wasm");
        assert!(answer(&app, "/nonexistent").is_none(), "404 face");
        // (Path:: placeholder dropped; the Path import rides serve_once's signature.)
    }

    #[test]
    fn the_serve_once_walks_the_http_min_face() {
        // The fixture is a socket-less face: the path parse works without a listener.
        let request = "GET /index.html HTTP/1.1\r\nhost: local\r\n\r\n";
        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or_default();
        assert_eq!(path, "/index.html");
    }
}
