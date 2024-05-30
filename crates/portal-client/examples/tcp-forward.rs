//! An example of TCP port forwarding via the portal service.

use std::time::Duration;

use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use futures_util::{Sink, SinkExt as _, Stream, StreamExt as _};
use portal_client::{IncomingClient, PortalService};
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tracing_subscriber::EnvFilter;

use counters::PerfCounters;

#[derive(Debug, Parser)]
struct Arguments {
    /// Server base URL
    #[arg(long, default_value = "ws://localhost:3000")]
    server: String,

    /// Mode to operate in
    #[command(subcommand)]
    mode: ForwardingMode,

    #[clap(flatten)]
    portal_params: PortalParams,
}

#[derive(Debug, Subcommand)]
enum ForwardingMode {
    /// Host a forwarded port
    Host(HostParams),
    /// Connect to a forwarded port
    Client(ClientParams),
}

#[derive(Debug, Clone, Parser)]
struct PortalParams {
    /// Tunnel service auth token
    #[arg(long)]
    auth_token: Option<String>,

    /// The service name
    #[arg(long, default_value = "")]
    service: String,

    /// Reconnect after connection ends
    #[arg(long)]
    reconnect: bool,
}

#[derive(Debug, Clone, Parser)]
struct HostParams {
    /// Port to forward
    #[arg(long, default_value = "localhost")]
    target_host: String,
    /// Port to forward
    #[arg(long, default_value_t = 22)]
    target_port: u16,
}

#[derive(Debug, Parser)]
struct ClientParams {
    /// Local interface to listen on
    #[arg(long, default_value = "0.0.0.0")]
    interface: String,
    /// Local port to listen on
    #[arg(long)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = Arguments::parse();

    // Allow the auth token to be passed in an environment variable.
    if args.portal_params.auth_token.is_none() {
        if let Some(env_token) = option_env!("PORTAL_TOKEN") {
            args.portal_params.auth_token = Some(env_token.to_owned());
        }
    }

    tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .without_time()
        .init();

    let service = PortalService::new(&args.server)?;

    match &args.mode {
        ForwardingMode::Host(host_params) => {
            run_host(&service, &args.portal_params, host_params).await?
        }
        ForwardingMode::Client(client_params) => {
            run_client(&service, &args.portal_params, client_params).await?
        }
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum ForwardError {
    /// An error during initial connection to the portal service.
    ///
    /// This may indicate a loss of network connectivity.
    #[error("portal connect error: {0}")]
    Connect(anyhow::Error),
    /// An error sending or receiving data to the portal service.
    ///
    /// This probably means the network connection was lost.
    #[error("portal dropped error: {0}")]
    Dropped(anyhow::Error),
    /// An error connecting via local TCP socket.
    #[error("tcp error: {0}")]
    TcpConnect(anyhow::Error),
    /// An error sending or receiving data on a local TCP socket.
    #[error("tcp error: {0}")]
    TcpDropped(anyhow::Error),
}

impl ForwardError {
    /// If the error may indicate a network disruption, sleep for a few seconds.
    ///
    /// If the error indicates that a working connection ended, do nothing.
    async fn retry_delay(&self) {
        match self {
            ForwardError::Connect(_) | ForwardError::TcpDropped(_) => {
                tokio::time::sleep(Duration::from_secs(10)).await
            }
            _ => (),
        }
    }
}

/// Make one or more host connections to the portal service.
///
/// If `reconnect` was specified, we will keep attempting to reconnect after
/// a connection failure or connection loss.
///
/// Only one connection will be established at a time; the previous connection
/// must terminate before a new one will be attempted.
async fn run_host(
    service: &PortalService,
    portal_params: &PortalParams,
    params: &HostParams,
) -> Result<(), ForwardError> {
    loop {
        if let Err(e) = forwarding_host(service, portal_params, params).await {
            e.retry_delay().await;
            tracing::info!("{e}");
        }
        if !portal_params.reconnect {
            return Ok(());
        }
    }
}

/// Make the host control connection to the portal service.
async fn forwarding_host(
    service: &PortalService,
    portal_params: &PortalParams,
    params: &HostParams,
) -> Result<(), ForwardError> {
    tracing::info!("running forwarding host");

    let auth_token = portal_params.auth_token.as_deref().unwrap_or_default();
    let mut tunnel_host = service
        .tunnel_host(auth_token)
        .await
        // FIXME: we should distinguish between authentication errors
        // and network errors; the former should not be retried.
        .map_err(|e| ForwardError::Connect(e.into()))?;

    // wait for the next incoming client connection.
    while let Some(incoming_client) = tunnel_host.next_client().await {
        let service_name = incoming_client.service_name();
        let port = match service_name.strip_prefix("port") {
            Some(port) => match port.parse::<u16>() {
                Ok(port) => port,
                Err(_) => {
                    tracing::info!("bad port number in service name {service_name}");
                    continue;
                }
            },
            None => {
                tracing::info!("unrecognized service name {service_name}");
                continue;
            }
        };

        // Handle the I/O forwarding in a separate task.
        tokio::spawn(forwarding_host_one(
            incoming_client,
            portal_params.clone(),
            params.clone(),
            port,
        ));
    }

    tracing::info!("forwarding host exiting");
    Ok(())
}

/// Handle a single forwarded connection.
async fn forwarding_host_one(
    incoming_client: IncomingClient,
    portal_params: PortalParams,
    params: HostParams,
    port: u16,
) -> Result<(), ForwardError> {
    let auth_token = portal_params.auth_token.as_deref().unwrap_or_default();
    let mut tunnel = incoming_client
        .connect(auth_token)
        .await
        .map_err(|e| ForwardError::Connect(e.into()))?;

    let host_port = format!("{}:{}", params.target_host, port);

    // Wait for the first forwarded bytes to arrive before creating the local TCP connection.
    //
    let message = match tunnel.next().await {
        Some(Ok(message)) => message,
        _ => {
            return Err(ForwardError::Dropped(anyhow!(
                "websocket error waiting for first bytes"
            )));
        }
    };

    let mut socket = TcpStream::connect(&host_port)
        .await
        .context("failed to open forwarding tcp stream")
        .map_err(ForwardError::TcpConnect)?;

    // Forward the first bytes.
    socket
        .write_all(&message)
        .await
        .context("failed to write message to TCP socket")
        .map_err(ForwardError::TcpDropped)?;

    let (tun_sender, tun_receiver) = tunnel.split();
    let (tcp_receiver, tcp_sender) = socket.into_split();

    let keepalive_period = Duration::from_secs(120);

    // FIXME: for consistency, this should return appropriate ForwardError
    tokio::select! {
        _ = tcp_to_tunnel(tcp_receiver, tun_sender, keepalive_period, None) => (),
        _ = tunnel_to_tcp(tun_receiver, tcp_sender, None) => (),
    }

    Ok(())
}

/// Wait for an incoming connection, then initiate a tunnel and forward data.
async fn run_client(
    service: &PortalService,
    portal_params: &PortalParams,
    params: &ClientParams,
) -> Result<(), ForwardError> {
    let local_port = format!("{}:{}", params.interface, params.port);
    tracing::info!("running forwarding client (listening: {})", local_port);

    let counters = PerfCounters::new();

    // Accept a single incoming connection.
    let listener = TcpListener::bind(&local_port)
        .await
        .context("failed to bind TCP port")
        .map_err(ForwardError::TcpConnect)?;
    loop {
        let (socket, _) = listener
            .accept()
            .await
            .context("failed to accept TCP connection")
            .map_err(ForwardError::TcpConnect)?;

        // We only forward one connection at a time.
        match client_connection(service, portal_params, socket, &counters).await {
            Err(e) => {
                tracing::info!("{e}");
                e.retry_delay().await;
            }
            _ => tracing::info!("connection ended"),
        }

        tracing::info!("{counters:#}");
        counters.clear();

        if !portal_params.reconnect {
            return Ok(());
        }
    }
}

async fn client_connection(
    service: &PortalService,
    portal_params: &PortalParams,
    socket: TcpStream,
    counters: &PerfCounters,
) -> Result<(), ForwardError> {
    // Connect to the tunnel.
    let auth_token = portal_params.auth_token.as_deref().unwrap_or_default();
    let tunnel = service
        .tunnel_client(auth_token, &portal_params.service)
        .await
        // FIXME: we should distinguish between authentication errors
        // and network errors; the former should not be retried.
        .map_err(|e| ForwardError::Connect(e.into()))?;

    let (tun_sender, tun_receiver) = tunnel.split();
    let (tcp_receiver, tcp_sender) = socket.into_split();

    let disable_keepalives = Duration::MAX;

    // FIXME: for consistency, this should return appropriate ForwardError
    tokio::select! {
        _ = tcp_to_tunnel(tcp_receiver, tun_sender, disable_keepalives, Some(counters)) => (),
        _ = tunnel_to_tcp(tun_receiver, tcp_sender, Some(counters)) => (),
    }

    Ok(())
}

/// Read data from a TCP socket and write it to a tunnel.
async fn tcp_to_tunnel<R, W>(
    mut tcp_receiver: R,
    mut ws_sender: W,
    keepalive_period: Duration,
    counters: Option<&PerfCounters>,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: Sink<Vec<u8>> + Unpin,
    <W as Sink<Vec<u8>>>::Error: std::error::Error + Send + Sync + 'static,
{
    use tokio::io::AsyncReadExt;

    loop {
        let mut buf = Vec::with_capacity(65536);

        // Race a timeout (for keepalives) with a TCP read.
        // Note: `AsyncReadExt::read_buf` is documented to be cancel-safe.
        let result = tokio::select! {
            tcp_result = tcp_receiver.read_buf(&mut buf) => Some(tcp_result),
            _ = tokio::time::sleep(keepalive_period) => None,
        };

        match result {
            Some(Err(e)) => {
                return Err(e).context("error reading from TCP socket");
            }
            Some(Ok(0)) => {
                // the socket read returned a length of 0 bytes.
                // This usually means the socket closed.
                return Ok(());
            }
            Some(Ok(_)) => {
                let msg_size = buf.len() as u64;
                ws_sender
                    .send(buf)
                    .await
                    .context("websocket write failed")?;
                if let Some(counters) = counters {
                    // "out" is from the TCP socket to the tunnel.
                    counters.count_out(msg_size);
                }
            }
            None => {
                // The sleep expired; send a websocket ping as a keepalive.
                // We (mis-)use an empty vec to trigger keepalive send.
                tracing::debug!("sending keepalive ping");
                ws_sender
                    .send(buf)
                    .await
                    .context("websocket write(ping) failed")?;
            }
        }
    }
}

/// Forward data unidirectionally from a tunnel to a TCP socket.
async fn tunnel_to_tcp<R, W, E>(
    mut ws_receiver: R,
    mut tcp_sender: W,
    counters: Option<&PerfCounters>,
) -> anyhow::Result<()>
where
    R: Stream<Item = Result<Vec<u8>, E>> + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    loop {
        let message_bytes = ws_receiver
            .next()
            .await
            .context("stream ended")?
            .context("stream error")?;
        //tracing::debug!("tunnel->tcp {message_bytes:?}");
        tcp_sender
            .write_all(&message_bytes)
            .await
            .context("failed to write message to TCP socket")?;
        if let Some(counters) = counters {
            // "in" is from the tunnel to the TCP socket.
            counters.count_in(message_bytes.len() as u64);
        }
    }
}

mod counters {
    use std::fmt::Display;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Performance counters for a network tunnel.
    ///
    /// Counts the number of bytes and the number of messages sent and received.
    #[derive(Debug, Default)]
    pub struct PerfCounters {
        in_bytes: AtomicU64,
        in_messages: AtomicU64,
        out_bytes: AtomicU64,
        out_messages: AtomicU64,
    }

    impl PerfCounters {
        /// Create new counters
        pub const fn new() -> Self {
            Self {
                in_bytes: AtomicU64::new(0),
                in_messages: AtomicU64::new(0),
                out_bytes: AtomicU64::new(0),
                out_messages: AtomicU64::new(0),
            }
        }

        /// Clear the counters
        pub fn clear(&self) {
            self.in_bytes.store(0, Ordering::Relaxed);
            self.in_messages.store(0, Ordering::Relaxed);
            self.out_bytes.store(0, Ordering::Relaxed);
            self.out_messages.store(0, Ordering::Relaxed);
        }

        /// Add an incoming message to the counters.
        pub fn count_in(&self, message_size: u64) {
            self.in_bytes.fetch_add(message_size, Ordering::Relaxed);
            self.in_messages.fetch_add(1, Ordering::Relaxed);
        }

        /// Add an outgoing message to the counters.
        pub fn count_out(&self, message_size: u64) {
            self.out_bytes.fetch_add(message_size, Ordering::Relaxed);
            self.out_messages.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Display for PerfCounters {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let in_bytes = self.in_bytes.load(Ordering::Relaxed);
            let in_messages = self.in_messages.load(Ordering::Relaxed);
            let in_avg = if in_messages == 0 {
                0
            } else {
                in_bytes / in_messages
            };
            let out_bytes = self.out_bytes.load(Ordering::Relaxed);
            let out_messages = self.out_messages.load(Ordering::Relaxed);
            let out_avg = if out_messages == 0 {
                0
            } else {
                out_bytes / out_messages
            };
            let break_char = if f.alternate() { ' ' } else { '\n' };
            write!(
                f,
                "in: {in_bytes} bytes, {in_messages} messages (avg msg size {in_avg}){break_char}out: {out_bytes} bytes, {out_messages} messages (avg msg size {out_avg})"
            )
        }
    }
}
