use super::render::render_prometheus_metrics;
use super::MetricsServerConfig;
use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use crate::ops::pm_table;
use crate::transport::SmuTransport;
use std::fmt::Write as _;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_HTTP_REQUEST_BYTES: usize = 4096;

const INDEX_HTML: &str = concat!(
    "<!DOCTYPE html>\n",
    "<html lang=\"en\">\n",
    "<head><meta charset=\"utf-8\"><title>zensmu exporter</title></head>\n",
    "<body>\n",
    "<h1>zensmu Prometheus exporter</h1>\n",
    "<p><a href=\"/metrics\">Metrics</a> | <a href=\"/healthz\">Health</a></p>\n",
    "</body>\n",
    "</html>\n",
);

/// Run the zero-dependency HTTP/1.1 Prometheus / OpenMetrics exporter until `stop_flag` is set.
///
/// Binds `config.listen_addr` in non-blocking mode, invokes `on_ready(local_addr)` with the
/// actual bound address (useful for ephemeral `127.0.0.1:0` binds), and serves:
/// - `GET /metrics` (or `HEAD /metrics`): live telemetry via [`pm_table::read_telemetry`]
/// - `GET /healthz`: `200 OK` (`ok\n`)
/// - `GET /`: `200 OK` HTML index linking to `/metrics`
/// - Any other path: `404 Not Found`
/// - Non-`GET`/`HEAD` methods: `405 Method Not Allowed`
pub fn serve_metrics<F>(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    config: &MetricsServerConfig,
    stop_flag: &Arc<AtomicBool>,
    mut on_ready: F,
) -> Result<()>
where
    F: FnMut(SocketAddr),
{
    let listener = TcpListener::bind(&config.listen_addr).map_err(|source| Error::Io {
        path: format!("tcp://{}", config.listen_addr),
        source,
    })?;
    let local_addr = listener.local_addr().map_err(|source| Error::Io {
        path: format!("tcp://{}", config.listen_addr),
        source,
    })?;
    listener.set_nonblocking(true).map_err(|source| Error::Io {
        path: format!("tcp://{local_addr}"),
        source,
    })?;

    on_ready(local_addr);

    while !stop_flag.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, _peer)) => {
                handle_connection(&mut stream, transport, hw, config);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {
                continue;
            }
            Err(_) => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }

    Ok(())
}

fn contains_header_terminator(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|w| w == b"\r\n\r\n") || bytes.windows(2).any(|w| w == b"\n\n")
}

fn handle_connection(
    stream: &mut TcpStream,
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    config: &MetricsServerConfig,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT));

    let mut buf = [0u8; MAX_HTTP_REQUEST_BYTES];
    let mut total = 0usize;
    while total < buf.len() {
        match stream.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if contains_header_terminator(&buf[..total]) {
                    break;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
            Err(_) => return,
        }
    }

    if total == 0 {
        return;
    }

    let request = String::from_utf8_lossy(&buf[..total]);
    let first_line = request.lines().next().map(str::trim).unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        let _ = write_http_response(
            stream,
            "400 Bad Request",
            "text/plain; charset=utf-8",
            "bad request\n",
            false,
            None,
        );
        return;
    };

    let head_only = match method {
        "GET" => false,
        "HEAD" => true,
        _ => {
            let _ = write_http_response(
                stream,
                "405 Method Not Allowed",
                "text/plain; charset=utf-8",
                "method not allowed\n",
                false,
                Some("Allow: GET, HEAD"),
            );
            return;
        }
    };

    let path = target.split('?').next().unwrap_or(target);
    match path {
        "/metrics" => match pm_table::read_telemetry(transport, hw) {
            Ok(snap) => {
                let body = render_prometheus_metrics(hw, &snap, config.include_per_core);
                let _ = write_http_response(
                    stream,
                    "200 OK",
                    PROMETHEUS_CONTENT_TYPE,
                    &body,
                    head_only,
                    None,
                );
            }
            Err(err) => {
                let body = format!("telemetry unavailable: {err}\n");
                let _ = write_http_response(
                    stream,
                    "503 Service Unavailable",
                    "text/plain; charset=utf-8",
                    &body,
                    head_only,
                    None,
                );
            }
        },
        "/healthz" => {
            let _ = write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                "ok\n",
                head_only,
                None,
            );
        }
        "/" => {
            let _ = write_http_response(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                INDEX_HTML,
                head_only,
                None,
            );
        }
        _ => {
            let _ = write_http_response(
                stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                "not found\n",
                head_only,
                None,
            );
        }
    }
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
    head_only: bool,
    extra_header: Option<&str>,
) -> std::io::Result<()> {
    let mut header = String::with_capacity(192);
    let _ = write!(header, "HTTP/1.1 {status}\r\n");
    let _ = write!(header, "Content-Type: {content_type}\r\n");
    let _ = write!(header, "Content-Length: {}\r\n", body.len());
    let _ = write!(header, "Cache-Control: no-store\r\n");
    if let Some(extra) = extra_header {
        let _ = write!(header, "{extra}\r\n");
    }
    header.push_str("Connection: close\r\n\r\n");

    stream.write_all(header.as_bytes())?;
    if !head_only {
        stream.write_all(body.as_bytes())?;
    }
    stream.flush()
}
