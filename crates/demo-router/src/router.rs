use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use axum::extract::ws::WebSocket;
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Extension;
use axum::{routing::get, Router};
use dashmap::DashMap;
use futures_util::StreamExt;
use tokio::net::TcpListener;

use crate::auth::{Auth, Claims};
use crate::monitor::IdleWebSocket;
use matic_portal_types::{Nexus, PortalId, Role, ServiceName};

/// This only contains host tunnels that are waiting for a client.
#[derive(Default)]
struct WaitingTunnels {
    /// Host control sockets.
    hosts: DashMap<PortalId, IdleWebSocket>,
    /// Client data sockets.
    clients: DashMap<Nexus, IdleWebSocket>,
}

impl WaitingTunnels {
    fn insert_host_ws(self: Arc<Self>, portal_id: PortalId, ws: WebSocket) {
        // For logging purposes we store a name with every idle socket.
        let name = portal_id.to_string();
        let idle_websocket = IdleWebSocket::new(ws, name);
        let previous = self.hosts.insert(portal_id, idle_websocket);
        if previous.is_some() {
            tracing::debug!("displaced a connected host");
        }
    }

    fn take_host_ws(&self, portal_id: PortalId) -> Option<(PortalId, IdleWebSocket)> {
        self.hosts.remove(&portal_id)
    }
}

pub struct PortalRouter {
    secret: String,
    socket_addr: SocketAddr,
    tcp_listener: Option<TcpListener>,
}

impl PortalRouter {
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
            socket_addr: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
            tcp_listener: None,
        }
    }

    pub fn with_addr(mut self, addr: impl ToSocketAddrs) -> Self {
        self.socket_addr = addr
            .to_socket_addrs()
            .expect("to_socket_addrs failed")
            .next()
            .expect("no server address found");
        self
    }

    pub async fn bind(&mut self) -> SocketAddr {
        if self.tcp_listener.is_none() {
            let listener = tokio::net::TcpListener::bind(self.socket_addr)
                .await
                .unwrap();
            self.tcp_listener = Some(listener);
        }
        self.tcp_listener.as_ref().unwrap().local_addr().unwrap()
    }

    pub async fn serve(mut self) {
        tracing::info!("portal router starting");

        let state = Arc::new(WaitingTunnels::default());
        let auth = Arc::new(Auth::new(&self.secret));

        // build our application with a route
        let app = Router::new()
            .route("/connect/host_control", get(connect_host))
            .route("/connect/host_accept/:nexus", get(host_accept))
            .route("/connect/client/:service", get(connect_client))
            .layer(Extension(auth))
            .with_state(state);

        self.bind().await;
        axum::serve(self.tcp_listener.unwrap(), app).await.unwrap();
    }
}

async fn connect_host(
    State(state): State<Arc<WaitingTunnels>>,
    auth_claims: Claims,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if let Err(status_code) = auth_claims.check(Role::Host) {
        return status_code.into_response();
    }
    let portal_id = auth_claims.portal_id;

    tracing::debug!("connect_host portal {portal_id}");

    // Note this runs tokio::spawn on handle_socket
    ws.on_upgrade(move |ws| async move { state.insert_host_ws(portal_id, ws) })
        .into_response()
}

async fn host_accept(
    State(state): State<Arc<WaitingTunnels>>,
    Path(nexus): Path<Nexus>,
    auth_claims: Claims,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if let Err(status_code) = auth_claims.check(Role::Host) {
        return status_code.into_response();
    }
    let portal_id = auth_claims.portal_id;

    tracing::debug!("host_accept portal {portal_id} nexus {}", nexus.raw_id());

    if let Err(status_code) = auth_claims.check(Role::Client) {
        return status_code.into_response();
    }
    let portal_id = auth_claims.portal_id;

    // If the desired nexus exists, take it from the hashmap.

    let Some((_, idle_websocket)) = state.take_host_ws(portal_id, nexus) else {
        tracing::info!("failed to find tunnel {portal_id}");
        return StatusCode::NOT_FOUND.into_response();
    };

    let Ok(mut host_ws) = idle_websocket.takeover().await else {
        tracing::debug!("tunnel {portal_id} is dead");
        return StatusCode::NOT_FOUND.into_response();
    };

    tracing::info!("connecting to tunnel {portal_id}:{service_name}");

    // Send a text message to the host with the identity of the client.
    let hello_message = format!(
        "connection to {portal_id}:{service_name} from {}",
        auth_claims.sub
    );
    if let Err(e) = host_ws
        .send(axum::extract::ws::Message::Text(hello_message))
        .await
    {
        tracing::error!("failed to send hello message to {portal_id}:{service_name}: {e}");
        return StatusCode::BAD_GATEWAY.into_response();
    }

    // Note this runs tokio::spawn to move the websocket handler into the background.
    ws.on_upgrade(move |socket| handle_client_websocket(socket, host_ws, portal_id, service_name))
        .into_response()
}

async fn connect_client(
    State(state): State<Arc<WaitingTunnels>>,
    Path(service_name): Path<ServiceName>,
    auth_claims: Claims,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if let Err(status_code) = auth_claims.check(Role::Client) {
        return status_code.into_response();
    }
    let portal_id = auth_claims.portal_id;

    tracing::debug!(
        "connect_client {} {portal_id}:{service_name}",
        auth_claims.sub
    );

    // If the desired portal+service exists, take it from the hashmap.

    // FIXME: don't take the host ws-- just send a message on it.
    let Some((_, idle_websocket)) = state.take_host_ws(portal_id) else {
        tracing::info!("failed to find tunnel {portal_id}");
        return StatusCode::NOT_FOUND.into_response();
    };

    let Ok(mut host_ws) = idle_websocket.takeover().await else {
        tracing::debug!("tunnel {portal_id} is dead");
        return StatusCode::NOT_FOUND.into_response();
    };

    tracing::info!("connecting to tunnel {portal_id}:{service_name}");

    // Send a text message to the host with the identity of the client.
    let hello_message = format!(
        "connection to {portal_id}:{service_name} from {}",
        auth_claims.sub
    );
    if let Err(e) = host_ws
        .send(axum::extract::ws::Message::Text(hello_message))
        .await
    {
        tracing::error!("failed to send hello message to {portal_id}:{service_name}: {e}");
        return StatusCode::BAD_GATEWAY.into_response();
    }

    // Note this runs tokio::spawn to move the websocket handler into the background.
    ws.on_upgrade(move |socket| handle_client_websocket(socket, host_ws, portal_id, service_name))
        .into_response()
}

async fn handle_client_websocket(
    client_ws: WebSocket,
    host_ws: WebSocket,
    portal_id: PortalId,
    service_name: ServiceName,
) {
    let (client_sender, client_receiver) = client_ws.split();
    let (host_sender, host_receiver) = host_ws.split();

    tracing::debug!("starting traffic forwarding {portal_id}:{service_name}");

    // Race the two async data-forwarding tasks. If either one exits, cancel the other and exit.
    // Cancel-safety is not an issue here, because we aren't looping: each of these futures runs
    // only once.
    tokio::select!(
        _ = host_receiver.forward(client_sender) => (),
        _ = client_receiver.forward(host_sender) => (),
    );
    tracing::info!("ending tunnel {portal_id}:{service_name}");
}
