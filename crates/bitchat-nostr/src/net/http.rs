//! Minimal HTTPS GET over the relay dialer (direct or Tor), used for the
//! daily geo relay list refresh.

use super::transport::{Dialer, Io, tls_connector};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("bad url")]
    BadUrl,
    #[error("connect: {0}")]
    Connect(#[from] super::transport::ConnectError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http status {0}")]
    Status(u16),
    #[error("malformed response")]
    Malformed,
    #[error("response too large")]
    TooLarge,
    #[error("timed out")]
    Timeout,
}

/// GETs an `https://` URL and returns the body (at most `max_bytes`).
pub async fn get(url: &str, dialer: &Dialer, max_bytes: usize) -> Result<Vec<u8>, HttpError> {
    tokio::time::timeout(TIMEOUT, get_inner(url, dialer, max_bytes))
        .await
        .map_err(|_| HttpError::Timeout)?
}

async fn get_inner(url: &str, dialer: &Dialer, max_bytes: usize) -> Result<Vec<u8>, HttpError> {
    let parsed = url::Url::parse(url).map_err(|_| HttpError::BadUrl)?;
    if parsed.scheme() != "https" {
        return Err(HttpError::BadUrl);
    }
    let host = parsed.host_str().ok_or(HttpError::BadUrl)?.to_string();
    let port = parsed.port_or_known_default().ok_or(HttpError::BadUrl)?;
    let path = match parsed.query() {
        Some(q) => format!("{}?{q}", parsed.path()),
        None => parsed.path().to_string(),
    };

    dialer.ready().await;
    let tcp = dialer.tcp_stream(&host, port).await?;
    let name =
        rustls::pki_types::ServerName::try_from(host.clone()).map_err(|_| HttpError::BadUrl)?;
    let mut tls: Io = Box::new(tls_connector().connect(name, tcp).await?);

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: bitchat-desktop\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
    );
    tls.write_all(request.as_bytes()).await?;

    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match tls.read(&mut buf).await {
            Ok(n) => n,
            // Servers often skip TLS close_notify after `Connection: close`.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
            Err(e) => return Err(e.into()),
        };
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if raw.len() > max_bytes + 64 * 1024 {
            return Err(HttpError::TooLarge);
        }
    }
    parse_response(&raw, max_bytes)
}

fn parse_response(raw: &[u8], max_bytes: usize) -> Result<Vec<u8>, HttpError> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or(HttpError::Malformed)?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| HttpError::Malformed)?;
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or(HttpError::Malformed)?;
    if status != 200 {
        return Err(HttpError::Status(status));
    }
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let body = if chunked {
        dechunk(body)?
    } else {
        body.to_vec()
    };
    if body.len() > max_bytes {
        return Err(HttpError::TooLarge);
    }
    Ok(body)
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    loop {
        let line_end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or(HttpError::Malformed)?;
        let size_str = std::str::from_utf8(&data[..line_end]).map_err(|_| HttpError::Malformed)?;
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| HttpError::Malformed)?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size + 2 {
            return Err(HttpError::Malformed);
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_chunked() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        assert_eq!(parse_response(plain, 100).unwrap(), b"hello");
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhel\r\n2\r\nlo\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked, 100).unwrap(), b"hello");
        let missing = b"HTTP/1.1 404 Not Found\r\n\r\n";
        assert!(matches!(
            parse_response(missing, 100),
            Err(HttpError::Status(404))
        ));
        assert!(matches!(parse_response(plain, 2), Err(HttpError::TooLarge)));
    }
}
