use std::fmt::Display;
use std::str::FromStr;
use std::sync::Arc;

use axum::extract::ws::WebSocket;
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{routing::get, Router};
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use demo_router::monitor::IdleWebSocket;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
struct TunnelId(u64);

impl FromStr for TunnelId {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(TunnelId(s.parse()?))
    }
}

// The display impl just prints the inner integer.
impl Display for TunnelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// This only contains host tunnels that are waiting for a client.
#[derive(Default)]
struct WaitingTunnels {
    tunnels: DashMap<TunnelId, IdleWebSocket>,
}

async fn insert_host_ws(state: Arc<WaitingTunnels>, tunnel_id: TunnelId, ws: WebSocket) {
    let idle_websocket = IdleWebSocket::new(ws, tunnel_id.to_string());
    let previous = state.tunnels.insert(tunnel_id, idle_websocket);
    if previous.is_some() {
        tracing::debug!("displaced a connected host");
    }
}

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt::init();

    tracing::info!("demo-router starting");

    let state = Arc::new(WaitingTunnels::default());

    // build our application with a route
    let app = Router::new()
        .route("/connect/host/:id", get(connect_host))
        .route("/connect/client/:id", get(connect_client))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn connect_host(
    State(state): State<Arc<WaitingTunnels>>,
    Path(tunnel_id): Path<TunnelId>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    tracing::debug!("connect_host {tunnel_id}");
    // Note this runs tokio::spawn on handle_socket
    ws.on_upgrade(move |ws| insert_host_ws(state, tunnel_id, ws))
        .into_response()
}

async fn connect_client(
    State(state): State<Arc<WaitingTunnels>>,
    Path(tunnel_id): Path<TunnelId>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    tracing::debug!("connect_client {tunnel_id}");

    // Find out if the desired host exists
    let Some((_, idle_websocket)) = state.tunnels.remove(&tunnel_id) else {
        tracing::info!("failed to find tunnel {tunnel_id}");
        return StatusCode::NOT_FOUND.into_response();
    };

    let Ok(host_ws) = idle_websocket.takeover().await else {
        tracing::debug!("tunnel {tunnel_id} is dead");
        return StatusCode::NOT_FOUND.into_response();
    };

    tracing::info!("connecting to tunnel {tunnel_id}");

    // // Send a text message to the host with the identity of the client.
    // let hello_message = format!("connection to tunnel {tunnel_id} from {}", client_name);
    // if let Err(e) = host_ws
    //     .send(axum::extract::ws::Message::Text(hello_message))
    //     .await
    // {
    //     tracing::error!("failed to send hello message to {tunnel_id}: {e}");
    //     return StatusCode::BAD_GATEWAY.into_response();
    // }

    // Note this runs tokio::spawn to move the websocket handler into the background.
    ws.on_upgrade(move |socket| handle_client_websocket(socket, host_ws, tunnel_id))
        .into_response()
}

async fn handle_client_websocket(client_ws: WebSocket, host_ws: WebSocket, tunnel_id: TunnelId) {
    let (client_sender, client_receiver) = client_ws.split();
    let (host_sender, host_receiver) = host_ws.split();

    tracing::debug!("starting traffic forwarding {tunnel_id}");

    // Race the two async data-forwarding tasks. If either one exits, cancel the other and exit.
    // Cancel-safety is not an issue here, because we aren't looping: each of these futures runs
    // only once.
    tokio::select!(
        _ = host_receiver.forward(client_sender) => (),
        _ = client_receiver.forward(host_sender) => (),
    );
    tracing::info!("ending tunnel {tunnel_id}");
}
