use std::collections::HashMap;
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
    // Baselines for per-torrent session upload, filled in by the first
    // torrent-get response the proxy forwards.
    let baselines: Arc<Mutex<Option<HashMap<String, u64>>>> = Arc::new(Mutex::new(None));

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let client = Arc::clone(&client);
                    let cache = Arc::clone(&stats_cache);
                    let baselines = Arc::clone(&baselines);
                    let hostname = hostname.clone();
                    std::thread::spawn(move || {
                        handle_conn(stream, &client, &cache, &baselines, &hostname)
                    });
                }
                // Transient accept errors (e.g. fd exhaustion) would
                // otherwise spin the accept loop.
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
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
    baselines: &Mutex<Option<HashMap<String, u64>>>,
    hostname: &str,
) {
    // Idle keep-alive connections are dropped after this. The dashboard
    // polls every 2 s, so pooled connections stay busy while the page is
    // open, and browsers discard pooled sockets the server has closed, so
    // an expiring idle connection costs at most one reconnect.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(75)));
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    loop {
        let Some((method, path, version, headers, body)) = read_request(&mut stream, &mut buf)
        else {
            return;
        };
        let keep_alive = wants_keep_alive(&version, &headers);

        // Reject requests that don't come from a loopback context: a browser
        // page on another origin can't read the response anyway (no CORS
        // headers), but it could still trigger calls like port-test, and a
        // DNS-rebinding page could otherwise read everything.
        let (status, payload) = if !request_is_local(&headers) {
            (
                403u16,
                json!({ "error": "forbidden origin" }).to_string(),
            )
        } else {
            match (method.as_str(), path.as_str()) {
                ("GET", "/") => (200u16, INDEX_HTML.replace("[hostname]", hostname)),
                ("POST", "/rpc") => proxy_rpc(&body, client, stats_cache, baselines),
                _ => (404, "not found".to_string()),
            }
        };

        let content_type = if payload.starts_with('<') {
            "text/html; charset=utf-8"
        } else {
            "application/json"
        };
        let header = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n",
            status,
            reason(status),
            content_type,
            payload.len(),
            if keep_alive { "keep-alive" } else { "close" }
        );
        if stream.write_all(header.as_bytes()).is_err()
            || stream.write_all(payload.as_bytes()).is_err()
            || !keep_alive
        {
            return;
        }
    }
}

fn proxy_rpc(
    body: &[u8],
    client: &RpcClient,
    stats_cache: &Mutex<Option<Value>>,
    baselines: &Mutex<Option<HashMap<String, u64>>>,
) -> (u16, String) {
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
        Ok(mut args) => {
            if method == "session-stats" {
                *stats_cache.lock().unwrap() = Some(args.clone());
            } else if method == "torrent-get" {
                inject_session_upload(&mut args, baselines);
            }
            (200, args.to_string())
        }
        Err(e) => (502, json!({ "error": e }).to_string()),
    }
}

/// Adds `uploadedSeedingSession` to each seeding torrent: bytes uploaded
/// since the first torrent-get of this process. Transmission only exposes
/// the all-time `uploadedEver`, so on that first call we snapshot it as
/// the baseline — but only for torrents already seeding; torrents that
/// start seeding later never get a baseline and show no badge.
fn inject_session_upload(args: &mut Value, baselines: &Mutex<Option<HashMap<String, u64>>>) {
    let Some(torrents) = args.get_mut("torrents").and_then(|t| t.as_array_mut()) else {
        return;
    };
    let mut guard = baselines.lock().unwrap();
    if guard.is_none() {
        let mut map = HashMap::new();
        for t in torrents.iter() {
            if t["status"].as_u64() == Some(6)
                && let (Some(h), Some(u)) = (t["hashString"].as_str(), t["uploadedEver"].as_u64())
            {
                map.insert(h.to_string(), u);
            }
        }
        *guard = Some(map);
    }
    let map = guard.as_ref().unwrap();
    for t in torrents.iter_mut() {
        if t["status"].as_u64() != Some(6) {
            continue;
        }
        if let (Some(h), Some(u)) = (t["hashString"].as_str(), t["uploadedEver"].as_u64())
            && let Some(base) = map.get(h)
        {
            t["uploadedSeedingSession"] = json!(u.saturating_sub(*base));
        }
    }
}

/// Parses one request out of `buf`, refilling it from the stream as needed.
/// Bytes past the end of the request (e.g. a pipelined next request) are
/// left in `buf` for the following call. Returns (method, path, HTTP
/// version, headers, body), or None when the connection should be closed.
fn read_request(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
) -> Option<(String, String, String, Vec<(String, String)>, Vec<u8>)> {
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 16 * 1024 {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.lines();
    let mut parts = lines.next()?.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let version = parts.next().unwrap_or("HTTP/1.0").to_string();

    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();

    let content_length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);

    let need = header_end + content_length;
    while buf.len() < need {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[header_end..need].to_vec();
    buf.drain(..need);

    Some((method, path, version, headers, body))
}

/// HTTP/1.1 connections stay open unless the client asks to close;
/// HTTP/1.0 closes unless it explicitly asks for keep-alive.
fn wants_keep_alive(version: &str, headers: &[(String, String)]) -> bool {
    let conn = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("connection"))
        .map(|(_, v)| v.to_ascii_lowercase());
    match conn.as_deref() {
        Some(v) if v.contains("close") => false,
        Some(v) if v.contains("keep-alive") => true,
        _ => version == "HTTP/1.1",
    }
}

fn request_is_local(headers: &[(String, String)]) -> bool {
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    match get("host") {
        Some(h) if host_is_loopback(h) => {}
        _ => return false,
    }
    // Browsers always send Origin on cross-site POSTs; it must point back
    // at the dashboard itself. No header at all means curl or a
    // same-origin request, which is fine.
    if let Some(origin) = get("origin") {
        let origin = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
            .unwrap_or(origin);
        let host = origin.split('/').next().unwrap_or("");
        if !host_is_loopback(host) {
            return false;
        }
    }
    true
}

fn host_is_loopback(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    matches!(name.trim_end_matches('.'), "127.0.0.1" | "localhost" | "::1")
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
        post_with_headers(port, path, "Host: 127.0.0.1\r\n", body)
    }

    fn post_with_headers(port: u16, path: &str, headers: &str, body: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST {} HTTP/1.1\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            path,
            headers,
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
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
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

    /// Reads one HTTP response (headers plus Content-Length body) without
    /// waiting for the connection to close.
    fn read_http_response(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            let n = stream.read(&mut chunk).unwrap();
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let len = head
            .lines()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while buf.len() < header_end + len {
            let n = stream.read(&mut chunk).unwrap();
            buf.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    #[test]
    fn test_dashboard_keep_alive_serves_multiple_requests() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::new(dir.path().join("log.txt"), false);
        let rpc_port = mock_rpc_server();
        let client = Arc::new(RpcClient::new(rpc_port, None, None));
        let cache = Arc::new(Mutex::new(None));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dash_port = listener.local_addr().unwrap().port();
        drop(listener);
        spawn(dash_port, client, cache, &logger).unwrap();

        let mut stream = TcpStream::connect(("127.0.0.1", dash_port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let body = r#"{"method":"session-stats","arguments":{}}"#;

        // HTTP/1.1 defaults to keep-alive: two requests on one connection.
        for _ in 0..2 {
            let req = format!(
                "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(req.as_bytes()).unwrap();
            let resp = read_http_response(&mut stream);
            assert!(resp.contains("200 OK"), "{}", resp);
            assert!(resp.contains("\"ok\":true"));
            assert!(resp.contains("keep-alive"));
        }

        // Connection: close is honored and ends the exchange.
        let req = format!(
            "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let resp = read_http_response(&mut stream);
        assert!(resp.contains("200 OK"), "{}", resp);
        let mut rest = String::new();
        stream.read_to_string(&mut rest).unwrap();
    }

    #[test]
    fn test_session_upload_baseline_injection() {
        let baselines = Mutex::new(None);

        // First call: only the seeding torrent gets a baseline; nothing
        // is injected for a still-downloading one.
        let mut first = json!({"torrents": [
            {"id": 1, "status": 6, "hashString": "aaa", "uploadedEver": 1000},
            {"id": 2, "status": 4, "hashString": "bbb", "uploadedEver": 2000}
        ]});
        inject_session_upload(&mut first, &baselines);
        assert_eq!(baselines.lock().unwrap().as_ref().unwrap().len(), 1);
        assert_eq!(
            first["torrents"][0]["uploadedSeedingSession"].as_u64(),
            Some(0)
        );
        assert!(first["torrents"][1].get("uploadedSeedingSession").is_none());

        // Second call: delta injected from the baseline; the now-seeding
        // "bbb" still has no baseline because it wasn't seeding on the
        // process's first call.
        let mut second = json!({"torrents": [
            {"id": 1, "status": 6, "hashString": "aaa", "uploadedEver": 1750},
            {"id": 2, "status": 6, "hashString": "bbb", "uploadedEver": 2200}
        ]});
        inject_session_upload(&mut second, &baselines);
        assert_eq!(
            second["torrents"][0]["uploadedSeedingSession"].as_u64(),
            Some(750)
        );
        assert!(second["torrents"][1].get("uploadedSeedingSession").is_none());
    }

    #[test]
    fn test_dashboard_rejects_non_loopback_host_and_origin() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::new(dir.path().join("log.txt"), false);
        let rpc_port = mock_rpc_server();
        let client = Arc::new(RpcClient::new(rpc_port, None, None));
        let cache = Arc::new(Mutex::new(None));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dash_port = listener.local_addr().unwrap().port();
        drop(listener);
        spawn(dash_port, client, cache, &logger).unwrap();

        // DNS-rebinding style request: wrong Host
        let resp = post_with_headers(
            dash_port,
            "/rpc",
            "Host: evil.example\r\n",
            r#"{"method":"session-stats","arguments":{}}"#,
        );
        assert!(resp.contains("403"), "{}", resp);

        // Cross-origin page POSTing to localhost: right Host, wrong Origin
        let resp = post_with_headers(
            dash_port,
            "/rpc",
            "Host: 127.0.0.1\r\nOrigin: https://evil.example\r\n",
            r#"{"method":"session-stats","arguments":{}}"#,
        );
        assert!(resp.contains("403"), "{}", resp);
    }
}
