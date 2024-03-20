use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::{Sink, Stream};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use url::Url;

#[derive(Debug, thiserror::Error)]
#[error("bad service url")]
pub struct UrlError;

#[derive(Debug, thiserror::Error)]
#[error("portal stream error")]
pub struct StreamError;

#[derive(Debug, thiserror::Error)]
#[error("portal sink error")]
pub struct SinkError;

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

    fn host_url(&self, portal_id: u64, service: &str) -> Url {
        let mut url = self.url.clone();
        url.set_path(&format!("/connect/host/{portal_id}/{service}"));
        url
    }

    fn client_url(&self, portal_id: u64, service: &str) -> Url {
        let mut url = self.url.clone();
        url.set_path(&format!("/connect/client/{portal_id}/{service}"));
        url
    }

    /// Create a host connection to the tunnel service.
    pub async fn tunnel_host(
        &self,
        token: &str,
        portal_id: u64,
        service: &str,
    ) -> Result<TunnelSocket, WsError> {
        let url = self.host_url(portal_id, service);
        tracing::debug!("tunnel_host {url}");
        let ws = websocket_connect(url.as_str(), token).await?;

        Ok(TunnelSocket { ws })
    }

    /// Create a client connection to the tunnel service.
    pub async fn tunnel_client(
        &self,
        token: &str,
        portal_id: u64,
        service: &str,
    ) -> Result<TunnelSocket, WsError> {
        let url = self.client_url(portal_id, service);
        tracing::debug!("tunnel_client {url}");
        let ws = websocket_connect(url.as_str(), token).await?;

        Ok(TunnelSocket { ws })
    }
}

type TcpWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

use pin_project::pin_project;

#[pin_project(project_replace)]
pub struct TunnelSocket {
    #[pin]
    ws: TcpWebSocket,
}

impl Stream for TunnelSocket {
    type Item = Result<Vec<u8>, StreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut ws = self.project().ws;

        // Incoming binary messages will be returned to the caller.
        // Other messages will be discarded.
        loop {
            let message = match ws.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(message))) => message,
                Poll::Ready(Some(Err(_))) => return Poll::Ready(Some(Err(StreamError))),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            };

            match message {
                Message::Text(_) => (),
                Message::Binary(message) => return Poll::Ready(Some(Ok(message))),
                _ => (),
            }
        }
    }
}

impl Sink<Vec<u8>> for TunnelSocket {
    type Error = SinkError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut ws = self.project().ws;
        ws.as_mut().poll_ready(cx).map_err(|_| SinkError)
    }

    fn start_send(self: Pin<&mut Self>, item: Vec<u8>) -> Result<(), Self::Error> {
        // We (mis-)use an empty Vec to signal that a keepalive should be sent.
        let item = if item.is_empty() {
            Message::Ping(item)
        } else {
            Message::Binary(item)
        };

        let mut ws = self.project().ws;
        ws.as_mut().start_send(item).map_err(|_| SinkError)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut ws = self.project().ws;
        ws.as_mut().poll_flush(cx).map_err(|_| SinkError)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut ws = self.project().ws;
        ws.as_mut().poll_close(cx).map_err(|_| SinkError)
    }
}

/// Make a websocket connection, using a bearer token.
async fn websocket_connect(url: &str, token: &str) -> Result<TcpWebSocket, WsError> {
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", token).parse().unwrap(),
    );
    let (bot_websocket, http_response) = connect_async(request).await?;
    tracing::debug!("got http response: {http_response:?}");
    Ok(bot_websocket)
}
