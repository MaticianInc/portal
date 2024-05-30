//! Structured messages used for communication from server to client.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::nexus::Nexus;

/// A portal message used to communicate with hosts and clients.
///
/// The portal server may send `Status` messages to a host or client at any time.
/// It will send `Incoming` messages only to a host control websocket, and will
/// send `Connected` messages only to a client websocket.
///
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ControlMessage {
    /// An informational message that may be useful for logging or debugging.
    Status(Cow<'static, str>),
    /// An announcement to the host that an incoming client connection has arrived.
    Incoming {
        /// Informational: the name of the client.
        client_name: String,
        /// The name of the service that the client has requested.
        ///
        /// This should be used by the host to determine whether to accept the connection,
        /// and what to do with the data once connected.
        // FIXME: use ServiceName type instead.
        service_name: String,
        /// The ID of the nexus assigned by the server.
        ///
        /// This value is required for the host to complete the connection.
        nexus: Nexus,
    },
    /// An announcement to the client that the connection to the host is established.
    ///
    /// Clients must not send data until this message is received.
    Connected,
}
