use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::Logger;
use crate::rpc::{RpcClient, is_method_allowed};

/// Spawns the local dashboard HTTP server on a background thread. Bound to
/// localhost only; only whitelisted read-only RPC methods are proxied through.
/// Returns the dashboard URL, or None if the port could not be bound.
pub fn spawn(
    port: u16,
    client: Arc<RpcClient>,
    stats_cache: Arc<Mutex<Option<Value>>>,
    logger: &Logger,
) -> Option<String> {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            logger.log(&format!(
                "Dashboard disabled: cannot bind 127.0.0.1:{}: {}",
                port, e
            ));
            return None;
        }
    };

    let hostname = detect_hostname();

    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let client = Arc::clone(&client);
            let cache = Arc::clone(&stats_cache);
            let hostname = hostname.clone();
            std::thread::spawn(move || handle_conn(stream, &client, &cache, &hostname));
        }
    });

    Some(format!("http://127.0.0.1:{}/", port))
}

pub fn open_in_browser(url: &str, logger: &Logger) {
    let result = if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn()
    } else {
        Command::new("xdg-open").arg(url).spawn()
    };
    match result {
        Ok(_) => logger.log(&format!("Dashboard available at {}", url)),
        Err(e) => logger.log(&format!(
            "Could not open browser ({}); dashboard is at {}",
            e, url
        )),
    }
}

fn detect_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

fn handle_conn(
    mut stream: TcpStream,
    client: &RpcClient,
    stats_cache: &Mutex<Option<Value>>,
    hostname: &str,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Some((method, path, body)) = read_request(&mut stream) else {
        return;
    };

    let (status, payload) = match (method.as_str(), path.as_str()) {
        ("GET", "/") => (200u16, INDEX_HTML.replace("[hostname]", hostname)),
        ("POST", "/rpc") => proxy_rpc(&body, client, stats_cache),
        _ => (404, "not found".to_string()),
    };

    let content_type = if payload.starts_with('<') {
        "text/html; charset=utf-8"
    } else {
        "application/json"
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason(status),
        content_type,
        payload.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
}

fn proxy_rpc(body: &[u8], client: &RpcClient, stats_cache: &Mutex<Option<Value>>) -> (u16, String) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({ "error": e.to_string() }).to_string()),
    };

    let method = parsed["method"].as_str().unwrap_or("");
    if !is_method_allowed(method) {
        return (
            403,
            json!({ "error": format!("method '{}' is not allowed", method) }).to_string(),
        );
    }

    let arguments = if parsed["arguments"].is_null() {
        json!({})
    } else {
        parsed["arguments"].clone()
    };

    match client.call(method, arguments) {
        Ok(args) => {
            if method == "session-stats" {
                *stats_cache.lock().unwrap() = Some(args.clone());
            }
            (200, args.to_string())
        }
        Err(e) => (502, json!({ "error": e }).to_string()),
    }
}

fn read_request(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 16 * 1024 {
            return None;
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.lines();
    let mut parts = lines.next()?.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let content_length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Some((method, path, body))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Bad Gateway",
    }
}

const INDEX_HTML: &str = include_str!("dashboard.html");

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn read_http_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]);
                let len = head
                    .lines()
                    .filter_map(|l| l.split_once(':'))
                    .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    return Some(buf);
                }
            }
        }
    }

    /// Mock Transmission RPC: replies 409 with a session id until the client
    /// echoes the session id back, then returns success. Used to verify the
    /// proxy enforces the method allowlist.
    fn mock_rpc_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut stream = stream;
                    while let Some(buf) = read_http_request(&mut stream) {
                        let req = String::from_utf8_lossy(&buf);
                        if req.contains("X-Transmission-Session-Id: sid") {
                            let body = r#"{"result":"success","arguments":{"ok":true}}"#;
                            let _ = stream.write_all(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                                    body.len(),
                                    body
                                )
                                .as_bytes(),
                            );
                        } else {
                            let _ = stream.write_all(
                                b"HTTP/1.1 409 Conflict\r\nX-Transmission-Session-Id: sid\r\nContent-Length: 0\r\n\r\n",
                            );
                        }
                    }
                });
            }
        });
        port
    }

    fn post(port: u16, path: &str, body: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST {} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{}",
            path,
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        resp
    }

    #[test]
    fn test_dashboard_serves_index_and_proxies_allowed_method() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::new(dir.path().join("log.txt"), false);
        let rpc_port = mock_rpc_server();
        let client = Arc::new(RpcClient::new(rpc_port, None, None));
        let cache = Arc::new(Mutex::new(None));

        // Bind a real dashboard port by retrying spawn on an OS-assigned one.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dash_port = listener.local_addr().unwrap().port();
        drop(listener);

        let url = spawn(dash_port, client, cache.clone(), &logger).unwrap();
        assert_eq!(url, format!("http://127.0.0.1:{}/", dash_port));

        // Index page
        let mut stream = TcpStream::connect(("127.0.0.1", dash_port)).unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK"));
        assert!(resp.contains("Transmission visualization"));
        assert!(!resp.contains("[hostname]"));

        // Allowed method is proxied
        let resp = post(
            dash_port,
            "/rpc",
            r#"{"method":"session-stats","arguments":{}}"#,
        );
        assert!(resp.contains("200 OK"), "{}", resp);
        assert!(resp.contains("\"ok\":true"));
        assert!(cache.lock().unwrap().is_some());

        // Disallowed method is rejected before reaching Transmission
        let resp = post(
            dash_port,
            "/rpc",
            r#"{"method":"torrent-remove","arguments":{"ids":[1]}}"#,
        );
        assert!(resp.contains("403"), "{}", resp);
        assert!(resp.contains("not allowed"));
    }
}
