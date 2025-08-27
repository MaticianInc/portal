//! A client for the portal service.

use std::fmt::{Debug, Display};
use std::io::{self, ErrorKind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Sink, SinkExt as _, Stream, StreamExt as _};
use matic_portal_types::{ControlMessage, Nexus};
use pin_project::pin_project;
use rustls::ClientConfig;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{
    connect_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream,
};
use url::Url;

// Publish this so that downstream users don't need to independently
// import the exact version of tungstenite just to inspect errors.
pub use tokio_tungstenite::tungstenite::Error as WsError;

// `rustls_platform_verifier` needs to call into JVM to work on android.
// `init_with_env` stores the reference to the `JNIEnv` in a global `OnceLock`,
// which is used to do the JVM calls. As such, we need to enforce that the library
// version for which `init_with_env` is called is the same as the one used here.
#[cfg(target_os = "android")]
pub use rustls_platform_verifier::android::init_with_env;

mod tunnel_io;

/// The amount of time a control socket can be idle before we send a keepalive ping.
const DEFAULT_KEEPALIVE_PERIOD: Duration = Duration::from_secs(120);

/// An malformed URL was passed to `PortalService::new()`
#[derive(Debug, thiserror::Error)]
#[error("bad service url")]
pub struct UrlError;

/// An error communicating with the Portal services.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    /// A timeout occurred while connecting.
    Timeout,
    /// An error occurred on the websocket.
    Websocket(#[from] WsError),
    /// Something went wrong with the portal control protocol.
    Protocol,
}

impl Display for PortalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => f.write_str("portal connection timeout"),
            Self::Websocket(WsError::Http(response)) if !response.status().is_success() => {
                write!(f, "http error {}", response.status())
            }
            Self::Websocket(ws_err) => {
                write!(f, "websocket error: {ws_err}")
            }
            Self::Protocol => f.write_str("portal protocol error"),
        }
    }
}

/// An object representing the portal service we want to connect to.
#[derive(Clone, Debug)]
pub struct PortalService {
    url: Url,
    keepalive_period: Duration,
}

impl PortalService {
    /// Specify the URL for the tunnel service.
    pub fn new(url: &str) -> Result<Self, UrlError> {
        let url = Url::parse(url).map_err(|_| UrlError)?;
        // FIXME: reject the url if it contains a path or any query parameters.
        Ok(Self {
            url,
            keepalive_period: DEFAULT_KEEPALIVE_PERIOD,
        })
    }

    /// Set the keepalive period.
    ///
    /// A host will automatically send keepalive ping messages if the
    /// socket is idle for this amount of time. If no response is received
    /// to the ping within the keepalive period, the connection will be closed.
    /// This means that it may take up to 2x the keepalive period to detect a
    /// network failure.
    ///
    /// The keepalive period is 30 seconds by default.
    ///
    /// To disable keepalives, use `Duration::MAX`.
    pub fn set_keepalive_period(&mut self, period: Duration) {
        self.keepalive_period = period;
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
    ///
    /// Any network errors will be returned immediately to the caller.
    /// It is the caller's responsibility to decide when to retry, if
    /// it intends to gracefully handle temporary network issues.
    pub async fn tunnel_host(&self, token: &str) -> Result<TunnelHost, PortalError> {
        let url = self.host_url();
        tracing::debug!("tunnel_host {url}");
        let ws = websocket_connect(url.as_str(), token).await?;

        Ok(TunnelHost {
            service: self.clone(),
            ws,
            keepalive_period: self.keepalive_period,
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
        timeout_duration: Duration,
    ) -> Result<TunnelSocket, PortalError> {
        let timeout_result =
            timeout(timeout_duration, self.tunnel_client_inner(token, service)).await;

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
                    } else {
                        tracing::warn!("unexpected control message: {control_message:?}");
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
                msg => {
                    tracing::debug!("incoming unknown ws message: {msg:?}");
                }
            }
        }

        Ok(TunnelSocket { ws })
    }
}

/// A host control connection to the portal service.
pub struct TunnelHost {
    service: PortalService,
    ws: TcpWebSocket,
    keepalive_period: Duration,
}

impl TunnelHost {
    /// Wait for a client to connect.
    pub async fn next_client(&mut self) -> Option<IncomingClient> {
        // When `true`, we have sent a keepalive message, and if we don't
        // receive a reply we will consider the connection failed.
        let mut connection_in_doubt = false;

        loop {
            // Loop sending keepalives until we receive a message.
            // This assumes that WebSocketStream is cancel-safe.
            let Ok(stream_result) = timeout(self.keepalive_period, self.ws.next()).await else {
                if connection_in_doubt {
                    // We have not received any messages for 2 keepalive periods.
                    // Consider the connection failed.
                    tracing::warn!("control connection keepalive timeout");
                    return None;
                } else {
                    connection_in_doubt = true;
                }
                tracing::trace!("sending keepalive ping");
                self.ws.send(Message::Ping(Bytes::new())).await.ok()?;
                continue;
            };

            let Some(Ok(message)) = stream_result else {
                tracing::debug!("next_client stream ending");
                return None;
            };
            // We received a message, so the connection is still good.
            connection_in_doubt = false;

            match message {
                Message::Text(txt_msg) => {
                    let Ok(control_message) = serde_json::from_str::<ControlMessage>(&txt_msg)
                    else {
                        tracing::warn!("malformed control message");
                        continue;
                    };
                    if matches!(control_message, ControlMessage::Incoming { .. }) {
                        return Some(IncomingClient::new(self.service.clone(), control_message));
                    } else {
                        tracing::warn!("unexpected control message: {control_message:?}");
                    }
                }
                Message::Binary(_) => {
                    tracing::warn!("ignoring binary message on host control socket");
                }
                Message::Close(Some(close)) => {
                    tracing::info!("control socket closed: {close}");
                }
                // Ignore all other message types.
                msg => {
                    tracing::debug!("incoming unknown ws message {msg:?}");
                }
            }
        }
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
    type Item = io::Result<Bytes>;

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
                Message::Pong(_) => {
                    tracing::debug!("received pong message");
                }
                _ => (),
            }
        }
    }
}

impl Sink<Bytes> for TunnelSocket {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut ws = self.project().ws;
        ws.as_mut()
            .poll_ready(cx)
            .map_err(|_| ErrorKind::BrokenPipe.into())
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        // We (mis-)use an empty Vec to signal that a keepalive should be sent.
        let item = if item.is_empty() {
            Message::Ping(Bytes::new())
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
    use rustls_platform_verifier::ConfigVerifierExt;

    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", token).parse().unwrap(),
    );
    let config = Arc::new(ClientConfig::with_platform_verifier().unwrap());
    let (websocket, http_response) =
        connect_async_tls_with_config(request, None, false, Some(Connector::Rustls(config)))
            .await?;
    tracing::debug!("got http response: {http_response:?}");
    Ok(websocket)
}
