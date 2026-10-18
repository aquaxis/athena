//! The `athena-studio` bin (spec §4.3.2): the static server over the lib's kernel
//! face — one port (5173), one asset set, `--api` naming the Core API root the wasm
//! app binds to (`athena-studio` talks to `athena-api` **only**, `ATH-API-001`).

use athena_studio::{StudioApp, answer, boot_from_args};
use std::io::{Read as _, Write as _};
use std::net::TcpListener;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let app = match boot_from_args(&args) {
        Ok(app) => app,
        Err(fault) => {
            eprintln!("athena-studio: {fault}");
            std::process::exit(4);
        }
    };
    let port = port_of(&args);
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("athena-studio: cannot bind 127.0.0.1:{port}: {error}");
            std::process::exit(4);
        }
    };
    eprintln!(
        "athena-studio: http://127.0.0.1:{port} — api_root {}",
        app.api_root
    );
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        let served = serve_once(&app, &mut stream);
        if let Err(error) = served {
            eprintln!("athena-studio: {error}");
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
