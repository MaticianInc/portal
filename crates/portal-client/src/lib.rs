//! A client for the portal service.

use std::fmt::Debug;
use std::io::{self, ErrorKind};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures_util::{Sink, Stream, StreamExt as _};
use matic_portal_types::{ControlMessage, Nexus};
use pin_project::pin_project;
use rustls::ClientConfig;
use rustls_platform_verifier::Verifier;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::{
    connect_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream,
};
use url::Url;

mod tunnel_io;

/// An malformed URL was passed to `PortalService::new()`
#[derive(Debug, thiserror::Error)]
#[error("bad service url")]
pub struct UrlError;

/// An error communicating with the Portal services.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    /// A timeout occurred while connecting.
    #[error("portal connection timeout")]
    Timeout,
    /// An error occurred on the websocket.
    #[error("websocket error")]
    Websocket(#[from] WsError),
    /// Something went wrong with the portal control protocol.
    #[error("portal protocol error")]
    Protocol,
}

/// An object representing the portal service we want to connect to.
#[derive(Clone, Debug)]
pub struct PortalService {
    url: Url,
}

impl PortalService {
    /// Specify the URL for the tunnel service.
    pub fn new(url: &str) -> Result<Self, UrlError> {
        let url = Url::parse(url).map_err(|_| UrlError)?;
        // FIXME: reject the url if it contains a path or any query parameters.
        Ok(Self { url })
    }

    fn host_url(&self) -> Url {
        let mut url = self.url.clone();
        url.set_path("/connect/host_control");
        url
    }

    fn host_accept_url(&self, nexus: Nexus) -> Url {
        let mut url = self.url.clone();
        url.set_path(&format!("/connect/host_accept/{}", nexus));
        url
    }

    fn client_url(&self, service: &str) -> Url {
        let mut url = self.url.clone();
        url.set_path(&format!("/connect/client/{service}"));
        url
    }

    /// Make a host connection to the service.
    pub async fn tunnel_host(&self, token: &str) -> Result<TunnelHost, PortalError> {
        let url = self.host_url();
        tracing::debug!("tunnel_host {url}");
        let ws = websocket_connect(url.as_str(), token).await?;

        Ok(TunnelHost {
            service: self.clone(),
            ws,
        })
    }

    /// Accept an incoming client connection.
    async fn accept_connection(
        &self,
        token: &str,
        nexus: Nexus,
    ) -> Result<TunnelSocket, PortalError> {
        let url = self.host_accept_url(nexus);
        tracing::debug!("accept_connection {url}");
        let ws = websocket_connect(url.as_str(), token).await?;

        Ok(TunnelSocket { ws })
    }

    /// Create a client connection to the portal service.
    ///
    /// A default timeout value will be used.
    pub async fn tunnel_client(
        &self,
        token: &str,
        service: &str,
    ) -> Result<TunnelSocket, PortalError> {
        self.tunnel_client_timeout(token, service, Duration::from_secs(10))
            .await
    }

    /// Create a client connection to the portal service.
    ///
    /// If a host is connected but doesn't accept the connection, the attempt will time out
    /// after the specified amount of time.
    pub async fn tunnel_client_timeout(
        &self,
        token: &str,
        service: &str,
        timeout: Duration,
    ) -> Result<TunnelSocket, PortalError> {
        let timeout_result =
            tokio::time::timeout(timeout, self.tunnel_client_inner(token, service)).await;

        match timeout_result {
            Err(_elapsed) => Err(PortalError::Timeout),
            Ok(result) => Ok(result?),
        }
    }

    async fn tunnel_client_inner(
        &self,
        token: &str,
        service: &str,
    ) -> Result<TunnelSocket, PortalError> {
        let url = self.client_url(service);
        tracing::debug!("tunnel_client {url}");
        let mut ws = websocket_connect(url.as_str(), token).await?;

        // A client must wait for a Connected message before it is allowed to transmit.
        // FIXME: add a built-in timeout here, so that naive callers won't wait forever.
        // FIXME: break out "wait for control message" into a separate function.
        while let Some(Ok(message)) = ws.next().await {
            match message {
                Message::Text(txt_msg) => {
                    let Ok(control_message) = serde_json::from_str::<ControlMessage>(&txt_msg)
                    else {
                        tracing::warn!("malformed control message");
                        continue;
                    };
                    if matches!(control_message, ControlMessage::Connected) {
                        return Ok(TunnelSocket { ws });
                    }
                }
                Message::Binary(_) => {
                    // We expect that the Connected message should happen before
                    // the host sends data. If this happens that message will not
                    // be delivered to the client because the TunnelSocket doesn't
                    // exist yet, so we will drop the connection.
                    tracing::error!("received binary message before Connected");
                    Err(PortalError::Protocol)?;
                }
                // Ignore all other message types.
                _ => {}
            }
        }

        Ok(TunnelSocket { ws })
    }
}

/// A host control connection to the portal service.
pub struct TunnelHost {
    service: PortalService,
    ws: TcpWebSocket,
}

impl TunnelHost {
    /// Wait for a client to connect.
    pub async fn next_client(&mut self) -> Option<IncomingClient> {
        while let Some(Ok(message)) = self.ws.next().await {
            match message {
                Message::Text(txt_msg) => {
                    let Ok(control_message) = serde_json::from_str::<ControlMessage>(&txt_msg)
                    else {
                        tracing::warn!("malformed control message");
                        continue;
                    };
                    if matches!(control_message, ControlMessage::Incoming { .. }) {
                        return Some(IncomingClient::new(self.service.clone(), control_message));
                    }
                }
                Message::Binary(_) => {
                    tracing::warn!("ignoring binary message on host control socket");
                }
                // Ignore all other message types.
                _ => {}
            }
        }
        None
    }
}

/// An incoming client connection.
pub struct IncomingClient {
    service: PortalService,
    control_message: ControlMessage,
}

impl IncomingClient {
    fn new(service: PortalService, control_message: ControlMessage) -> Self {
        assert!(matches!(control_message, ControlMessage::Incoming { .. }));
        Self {
            service,
            control_message,
        }
    }

    /// Return the service name requested by the client.
    ///
    // FIXME: use ServiceName type instead.
    pub fn service_name(&self) -> &str {
        let ControlMessage::Incoming { service_name, .. } = &self.control_message else {
            panic!("IncomingClient wrong control_message variant");
        };
        service_name
    }

    /// Accept this connection by connecting a data socket to the client.
    pub async fn connect(self, token: &str) -> Result<TunnelSocket, PortalError> {
        let ControlMessage::Incoming { nexus, .. } = self.control_message else {
            panic!("IncomingClient wrong control_message variant");
        };
        self.service.accept_connection(token, nexus).await
    }
}

type TcpWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A data connection to the portal service.
#[pin_project(project_replace)]
pub struct TunnelSocket {
    #[pin]
    ws: TcpWebSocket,
}

// This doesn't display any useful state; it's just here so
// unit tests can call .unwrap_err() on attempted connections.
impl Debug for TunnelSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TunnelSocket").finish()
    }
}

impl Stream for TunnelSocket {
    type Item = io::Result<Vec<u8>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut ws = self.project().ws;

        // Incoming binary messages will be returned to the caller.
        // Other messages will be discarded.
        loop {
            let message = match ws.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(message))) => message,
                Poll::Ready(Some(Err(_))) => {
                    return Poll::Ready(Some(Err(ErrorKind::BrokenPipe.into())))
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            };

            match message {
                Message::Text(text) => tracing::info!("service message: {text}"),
                Message::Binary(message) => return Poll::Ready(Some(Ok(message))),
                Message::Ping(_) => {
                    tracing::debug!("received ping message");
                }
                Message::Pong(message) => {
                    // We expect a Pong message to contain 8 bytes, that contain
                    // the f64 timestamp we sent in a Ping.
                    if let Ok(ts_bytes) = <[u8; 8]>::try_from(message) {
                        let ping_timestamp = f64::from_be_bytes(ts_bytes);
                        let now = get_timestamp();
                        let delta_time = 1000.0 * (now - ping_timestamp);
                        tracing::debug!("ping-pong time {delta_time} ms")
                    }
                }
                _ => (),
            }
        }
    }
}

impl Sink<Vec<u8>> for TunnelSocket {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut ws = self.project().ws;
        ws.as_mut()
            .poll_ready(cx)
            .map_err(|_| ErrorKind::BrokenPipe.into())
    }

    fn start_send(self: Pin<&mut Self>, item: Vec<u8>) -> Result<(), Self::Error> {
        // We (mis-)use an empty Vec to signal that a keepalive should be sent.
        let item = if item.is_empty() {
            let timestamp = get_timestamp();
            Message::Ping(timestamp.to_be_bytes().into())
        } else {
            Message::Binary(item)
        };

        let mut ws = self.project().ws;
        ws.as_mut()
            .start_send(item)
            .map_err(|_| ErrorKind::BrokenPipe.into())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut ws = self.project().ws;
        ws.as_mut()
            .poll_flush(cx)
            .map_err(|_| ErrorKind::BrokenPipe.into())
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut ws = self.project().ws;
        ws.as_mut()
            .poll_close(cx)
            .map_err(|_| ErrorKind::BrokenPipe.into())
    }
}

/// Make a websocket connection, using a bearer token.
async fn websocket_connect(url: &str, token: &str) -> Result<TcpWebSocket, WsError> {
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", token).parse().unwrap(),
    );
    let config = Arc::new(
        ClientConfig::builder()
            .dangerous() // The `Verifier` we're using is actually safe
            .with_custom_certificate_verifier(Arc::new(Verifier::new()))
            .with_no_client_auth(),
    );
    let (websocket, http_response) =
        connect_async_tls_with_config(request, None, false, Some(Connector::Rustls(config)))
            .await?;
    tracing::debug!("got http response: {http_response:?}");
    Ok(websocket)
}

static TIMESTAMP_BASE: OnceLock<Instant> = OnceLock::new();

/// Get a monotonic timestamp, in f64 seconds.
fn get_timestamp() -> f64 {
    let base = TIMESTAMP_BASE.get_or_init(Instant::now);
    base.elapsed().as_secs_f64()
}
