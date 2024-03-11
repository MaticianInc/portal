use std::sync::Arc;

use axum::extract::ws::WebSocket;
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Extension;
use axum::{routing::get, Router};
use dashmap::DashMap;
use demo_router::auth::{Auth, JwtClaims, Role};
use demo_router::tunnelid::PortalId;
use futures_util::StreamExt;

use demo_router::monitor::IdleWebSocket;

/// This only contains host tunnels that are waiting for a client.
#[derive(Default)]
struct WaitingTunnels {
    tunnels: DashMap<PortalId, IdleWebSocket>,
}

async fn insert_host_ws(state: Arc<WaitingTunnels>, portal_id: PortalId, ws: WebSocket) {
    let idle_websocket = IdleWebSocket::new(ws, portal_id.to_string());
    let previous = state.tunnels.insert(portal_id, idle_websocket);
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

    let Ok(auth_secret) = std::env::var("AUTH_SECRET") else {
        println!("Authentication secret missing. Please set AUTH_SECRET");
        return;
    };
    let auth = Arc::new(Auth::new(&auth_secret));

    // build our application with a route
    let app = Router::new()
        .route("/connect/host/:id", get(connect_host))
        .route("/connect/client/:id", get(connect_client))
        .layer(Extension(auth))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn connect_host(
    State(state): State<Arc<WaitingTunnels>>,
    Path(portal_id): Path<PortalId>,
    auth_claims: JwtClaims,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if let Err(status_code) = auth_claims.check(Role::Host, portal_id) {
        return status_code.into_response();
    }

    tracing::debug!("connect_host {portal_id}");
    // Note this runs tokio::spawn on handle_socket
    ws.on_upgrade(move |ws| insert_host_ws(state, portal_id, ws))
        .into_response()
}

async fn connect_client(
    State(state): State<Arc<WaitingTunnels>>,
    Path(portal_id): Path<PortalId>,
    auth_claims: JwtClaims,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if let Err(status_code) = auth_claims.check(Role::Client, portal_id) {
        return status_code.into_response();
    }

    tracing::debug!("connect_client {} {portal_id}", auth_claims.sub);

    // Find out if the desired host exists
    let Some((_, idle_websocket)) = state.tunnels.remove(&portal_id) else {
        tracing::info!("failed to find tunnel {portal_id}");
        return StatusCode::NOT_FOUND.into_response();
    };

    let Ok(host_ws) = idle_websocket.takeover().await else {
        tracing::debug!("tunnel {portal_id} is dead");
        return StatusCode::NOT_FOUND.into_response();
    };

    tracing::info!("connecting to tunnel {portal_id}");

    // // Send a text message to the host with the identity of the client.
    // let hello_message = format!("connection to tunnel {portal_id} from {}", client_name);
    // if let Err(e) = host_ws
    //     .send(axum::extract::ws::Message::Text(hello_message))
    //     .await
    // {
    //     tracing::error!("failed to send hello message to {portal_id}: {e}");
    //     return StatusCode::BAD_GATEWAY.into_response();
    // }

    // Note this runs tokio::spawn to move the websocket handler into the background.
    ws.on_upgrade(move |socket| handle_client_websocket(socket, host_ws, portal_id))
        .into_response()
}

async fn handle_client_websocket(client_ws: WebSocket, host_ws: WebSocket, portal_id: PortalId) {
    let (client_sender, client_receiver) = client_ws.split();
    let (host_sender, host_receiver) = host_ws.split();

    tracing::debug!("starting traffic forwarding {portal_id}");

    // Race the two async data-forwarding tasks. If either one exits, cancel the other and exit.
    // Cancel-safety is not an issue here, because we aren't looping: each of these futures runs
    // only once.
    tokio::select!(
        _ = host_receiver.forward(client_sender) => (),
        _ = client_receiver.forward(host_sender) => (),
    );
    tracing::info!("ending tunnel {portal_id}");
}
