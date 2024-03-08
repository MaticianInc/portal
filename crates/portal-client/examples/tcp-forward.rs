use anyhow::Context;
use clap::{Parser, Subcommand};
use futures_util::{Sink, SinkExt as _, Stream, StreamExt as _};
use portal_client::PortalService;
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Arguments {
    /// Server base URL
    #[arg(long, default_value = "ws://localhost:3000")]
    server: String,

    /// Mode to operate in
    #[command(subcommand)]
    mode: ForwardingMode,
}

#[derive(Debug, Subcommand)]
enum ForwardingMode {
    /// Host a forwarded port
    Host(HostParams),
    /// Connect to a forwarded port
    Client(ClientParams),
}

#[derive(Debug, Parser)]
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
    #[arg(long, default_value = "127.0.0.1")]
    interface: String,
    /// Local port to listen on
    #[arg(long)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Arguments::parse();

    tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .without_time()
        .init();

    let service = PortalService::new(&args.server)?;

    match args.mode {
        ForwardingMode::Host(host_params) => forwarding_host(service, host_params).await?,
        ForwardingMode::Client(client_params) => run_client(service, client_params).await?,
    }

    Ok(())
}

async fn forwarding_host(service: PortalService, params: HostParams) -> anyhow::Result<()> {
    let host_port = format!("{}:{}", params.target_host, params.target_port);
    tracing::info!("running forwarding host (target: {})", host_port);

    let mut tunnel = service
        .tunnel_host("mytoken", "1234")
        .await
        .context("failed to connect to tunnel server")?;

    // Wait for the first forwarded bytes to arrive before creating the local TCP connection.
    let message = tunnel
        .next()
        .await
        .context("no message received")? // .next() returned None (shouldn't happen?)
        .context("error on websocket stream")?; // .next() returned Some(Err(...))

    let mut socket = TcpStream::connect(&host_port)
        .await
        .context("failed to open forwarding tcp stream")?;

    // Forward the first bytes.
    socket
        .write_all(&message)
        .await
        .context("failed to write message to TCP socket")?;

    let (tun_sender, tun_receiver) = tunnel.split();
    let (tcp_receiver, tcp_sender) = socket.into_split();

    tokio::select! {
        _ = tcp_to_tunnel(tcp_receiver, tun_sender) => (),
        _ = tunnel_to_tcp(tun_receiver, tcp_sender) => (),
    }

    Ok(())
}

/// Wait for an incoming connection, then initiate a tunnel and forward data.
async fn run_client(service: PortalService, params: ClientParams) -> anyhow::Result<()> {
    let local_port = format!("{}:{}", params.interface, params.port);
    tracing::info!("running forwarding client (listening: {})", local_port);

    // Accept a single incoming connection.
    let listener = TcpListener::bind(&local_port).await?;
    let (socket, _) = listener.accept().await?;

    // Connect to the tunnel.
    let tunnel = service
        .tunnel_client("mytoken", "1234")
        .await
        .context("failed to connect to tunnel server")?;

    let (tun_sender, tun_receiver) = tunnel.split();
    let (tcp_receiver, tcp_sender) = socket.into_split();

    tokio::select! {
        _ = tcp_to_tunnel(tcp_receiver, tun_sender) => (),
        _ = tunnel_to_tcp(tun_receiver, tcp_sender) => (),
    }

    Ok(())
}

/// Read data from a TCP socket and write it to a tunnel.
async fn tcp_to_tunnel<R, W>(mut tcp_receiver: R, mut ws_sender: W) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: Sink<Vec<u8>> + Unpin,
    <W as Sink<Vec<u8>>>::Error: std::error::Error + Send + Sync + 'static,
{
    use tokio::io::AsyncReadExt;

    loop {
        let mut buf = Vec::new();
        let bytes_read = tcp_receiver
            .read_buf(&mut buf)
            .await
            .context("error reading from TCP socket")?;
        if bytes_read == 0 {
            // This usually means the socket closed.
            return Ok(());
        }
        // copy data from buffer. FIXME: is there a way to do this without copying?
        //let tx_buf = buf.to_vec();
        //tracing::debug!("tcp->ws {tx_buf:?}");
        // write data to websocket
        ws_sender
            .send(buf)
            .await
            .context("websocket write failed")?;
    }
}

/// Forward data unidirectionally from a tunnel to a TCP socket.
async fn tunnel_to_tcp<R, W, E>(mut ws_receiver: R, mut tcp_sender: W) -> anyhow::Result<()>
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
    }
}
