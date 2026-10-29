//! Opening relay websockets, directly or through Tor.

use super::tor::Tor;
use arti_client::{ErrorKind, HasKind};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{WebSocketStream, client_async_with_config};

pub trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncIo for T {}

pub type Io = Box<dyn AsyncIo>;
pub type Socket = WebSocketStream<Io>;

/// Websocket message cap (`nostrInboundMaxFrameBytes`).
pub const MAX_WS_MESSAGE_BYTES: usize = 512 * 1024;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How TCP connections to relays are made.
#[derive(Clone)]
pub enum Dialer {
    /// Straight to the relay; relays see this device's IP address.
    Direct,
    /// Through Tor; waits for Tor rather than falling back.
    Tor(Tor),
    /// Never connects: networking paused.
    Offline,
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("unusable relay address")]
    BadUrl,
    #[error("name lookup failed")]
    Dns,
    #[error("connection failed: {0}")]
    Io(#[from] io::Error),
    #[error("websocket handshake failed: {0}")]
    Handshake(String),
    #[error("timed out")]
    Timeout,
    #[error("tor: {0}")]
    Tor(String),
}

impl ConnectError {
    /// DNS failures and rejected handshakes are not retried quickly (iOS
    /// treats them as permanent and cools down for 10 minutes).
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            ConnectError::BadUrl | ConnectError::Dns | ConnectError::Handshake(_)
        )
    }
}

impl Dialer {
    async fn tcp(&self, host: &str, port: u16) -> Result<Io, ConnectError> {
        match self {
            Dialer::Direct => {
                let addrs: Vec<_> = tokio::net::lookup_host((host, port))
                    .await
                    .map_err(|_| ConnectError::Dns)?
                    .collect();
                if addrs.is_empty() {
                    return Err(ConnectError::Dns);
                }
                Ok(Box::new(TcpStream::connect(&addrs[..]).await?))
            }
            Dialer::Tor(tor) => match tor.connect(host, port).await {
                Ok(stream) => Ok(Box::new(stream)),
                Err(e) if e.kind() == ErrorKind::RemoteHostNotFound => Err(ConnectError::Dns),
                Err(e) => Err(ConnectError::Tor(e.to_string())),
            },
            Dialer::Offline => Err(ConnectError::Io(std::io::Error::other("offline"))),
        }
    }

    /// A raw TCP (or Tor) stream to `host:port`, for non-websocket use.
    pub async fn tcp_stream(&self, host: &str, port: u16) -> Result<Io, ConnectError> {
        self.tcp(host, port).await
    }

    /// Waits until the dialer can be used (Tor bootstrapped).
    pub async fn ready(&self) {
        match self {
            Dialer::Direct => {}
            Dialer::Tor(tor) => tor.wait_ready().await,
            Dialer::Offline => std::future::pending().await,
        }
    }
}

pub fn tls_connector() -> TlsConnector {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring supports the default protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

/// Connects to a normalized `ws://` or `wss://` relay URL.
pub async fn connect(
    url: &str,
    dialer: &Dialer,
    tls: &TlsConnector,
) -> Result<Socket, ConnectError> {
    let parsed = url::Url::parse(url).map_err(|_| ConnectError::BadUrl)?;
    let host = parsed
        .host_str()
        .ok_or(ConnectError::BadUrl)?
        .trim_matches(['[', ']'])
        .to_string();
    let port = parsed.port_or_known_default().ok_or(ConnectError::BadUrl)?;
    let secure = match parsed.scheme() {
        "wss" => true,
        "ws" => false,
        _ => return Err(ConnectError::BadUrl),
    };

    // Waiting for Tor is not part of the connect timeout.
    dialer.ready().await;
    let attempt = async {
        let tcp = dialer.tcp(&host, port).await?;
        let io: Io = if secure {
            let name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|_| ConnectError::BadUrl)?;
            Box::new(tls.connect(name, tcp).await?)
        } else {
            tcp
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_WS_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_WS_MESSAGE_BYTES));
        let (socket, _) = client_async_with_config(url, io, Some(config))
            .await
            .map_err(|e| ConnectError::Handshake(e.to_string()))?;
        Ok(socket)
    };
    tokio::time::timeout(CONNECT_TIMEOUT, attempt)
        .await
        .map_err(|_| ConnectError::Timeout)?
}
