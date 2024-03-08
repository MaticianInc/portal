use worker::{
    console_log, durable_object, Env, Request, Response, State, WebSocket,
    WebSocketIncomingMessage, WebSocketPair,
};

use crate::tunnel_id::TunnelId;

/// An enum describing the routes that the DurableRouter can handle
#[derive(Debug)]
enum ServiceRoute {
    Host(TunnelId),
    Client(TunnelId),
}

#[durable_object]
pub struct DurableRouter {
    state: State,
    _env: Env, // access `Env` across requests, use inside `fetch`
}

#[durable_object]
impl DurableObject for DurableRouter {
    fn new(state: State, env: Env) -> Self {
        Self { state, _env: env }
    }

    async fn fetch(&mut self, req: Request) -> worker::Result<Response> {
        let path = req.path();
        console_log!("DurableRouter fetch {path}");

        let route = Self::parse_path(&path);
        console_log!("parsed {path} -> {route:?}");
        match route {
            None => {
                console_log!("DurableRouter fetch 404");
                Response::error("Not Found", 404)
            }
            Some(ServiceRoute::Client(id)) => self.handle_client(id).await,
            Some(ServiceRoute::Host(id)) => self.handle_host(id).await,
        }
    }

    async fn websocket_message(&mut self, ws: WebSocket, message: WebSocketIncomingMessage) -> worker::Result<()> {
        let tags = self.state.get_tags(&ws);
        console_log!("incoming message from ws with tags {tags:?}");

        // Figure out if the sender is a host or client.
        for tag in tags {
            if tag == "h" {
                self.message_from_host(message);
                break;
            } else if let Some(conn_num) = tag.strip_prefix("cs ") {
                if let Ok(conn_num) = conn_num.parse::<u32>() {
                    self.message_from_client(message, conn_num);
                    break;
                }
            }
            console_log!("unrecognized tag {tag}");
        }
        Ok(())
    }
}

impl DurableRouter {
    fn parse_path(path: &str) -> Option<ServiceRoute> {
        if let Some(id) = path.strip_prefix("/echo_client/") {
            return id.parse().ok().map(ServiceRoute::Client);
        }
        if let Some(id) = path.strip_prefix("/echo_host/") {
            return id.parse().ok().map(ServiceRoute::Host);
        }
        None
    }

    /// Handle incoming host request.
    ///
    /// We will store a new host websocket, and return its peer.
    /// Nothing more will happen until a client connects.
    async fn handle_host(&mut self, _id: TunnelId) -> worker::Result<Response> {
        let pair = WebSocketPair::new()?;
        let host_ws = pair.client;
        let server_ws = pair.server;

        self.state.accept_websocket_with_tags(&server_ws, &["h"]);

        server_ws
            .send_with_str("hello host from cf-worker")
            .unwrap();

        Response::from_websocket(host_ws)
    }

    /// Handle incoming client request.
    ///
    /// If the `DurableRouter` is healthy, we will generate a new websocket
    /// for this client, and move messages back and forth between the host
    /// and this client.
    async fn handle_client(&mut self, _id: TunnelId) -> worker::Result<Response> {
        // We should only allow this connection if the server has already connected.
        let host_socket = self.state.get_websockets_with_tag("h");
        if host_socket.is_empty() {
            console_log!("no host socket found");
            return Response::error("tunnel host not available", 503);
        }

        let pair = WebSocketPair::new()?;
        let client_ws = pair.client;
        let server_ws = pair.server;
        let conn_id = crate::random_channel();
        let client_tag = format!("cs {conn_id}");
        self.state.accept_websocket_with_tags(&server_ws, &[&client_tag]);
        server_ws
            .send_with_str(format!("hello client {conn_id} from cf-worker"))
            .unwrap();

        Response::from_websocket(client_ws)
    }

    /// Handle a message from the host
    fn message_from_host(&self, message: WebSocketIncomingMessage) {
        match message {
            WebSocketIncomingMessage::String(_) => {
                console_log!("ignoring string message from host");
            }
            WebSocketIncomingMessage::Binary(mut msg) => {
                // The connection number is stored in the last 4 bytes.
                let Some(conn_num_offset) = msg.len().checked_sub(4) else {
                    console_log!("malformed message, bad length");
                    return;
                };
                // Remove the connection number from the end of the message.
                let conn_num_bytes = msg.split_off(conn_num_offset);
                let conn_num = u32::from_be_bytes(conn_num_bytes.try_into().unwrap());
                let client_tag = format!("cs {conn_num}");
                let mut client_sockets = self.state.get_websockets_with_tag(&client_tag);
                // FIXME: decide what to do if >1 matching client sockets are found.
                let Some(client_socket) = client_sockets.pop() else {
                    console_log!("no host socket found");
                    return;
                };
                if let Err(e) = client_socket.send_with_bytes(&msg) {
                    console_log!("error sending to client: {e}");
                }
            }
        }
    }

    /// Handle a message from the client
    fn message_from_client(&self, message: WebSocketIncomingMessage, conn_num: u32) {
        match message {
            WebSocketIncomingMessage::String(_) => {
                console_log!("ignoring string message from client");
            }
            WebSocketIncomingMessage::Binary(mut msg) => {
                // tack the connection number on the end.
                msg.extend_from_slice(&conn_num.to_be_bytes());
                // send the encoded message to the host.
                let mut host_sockets = self.state.get_websockets_with_tag("h");
                // FIXME: decide what to do if >1 host sockets are found.
                let Some(host_socket) = host_sockets.pop() else {
                    console_log!("no host socket found");
                    return;
                };
                if let Err(e) = host_socket.send_with_bytes(&msg) {
                    console_log!("error sending to host: {e}");
                }
            }
        }
    }
}
