use std::borrow::Cow;

use anyhow::Context;
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use portal_client::PortalService;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Arguments {
    /// Server base URL
    #[arg(long, default_value = "ws://localhost:3000")]
    server: String,

    /// The tunnel identifier
    #[arg(long, default_value = "1234")]
    portal_id: u64,

    /// Tunnel service auth token
    #[arg(long)]
    auth_token: Option<String>,
}

async fn run_host(args: &Arguments) -> anyhow::Result<()> {
    tracing::info!("running echo host");
    let service = PortalService::new(&args.server)?;
    let mut tunnel = service
        .tunnel_host(
            args.auth_token.as_deref().unwrap_or_default(),
            &args.portal_id.to_string(),
        )
        .await
        .context("failed to connect to server")?;

    loop {
        let message = tunnel
            .next()
            .await
            .context("no message received")? // .next() returned None (shouldn't happen?)
            .context("error on websocket stream")?; // .next() returned Some(Err(...))

        let printable_message = match std::str::from_utf8(&message) {
            Ok(text) => Cow::from(text),
            Err(_) => Cow::from(format!("binary message ({} bytes)", message.len())),
        };
        tracing::info!("message from client: {printable_message}");

        // Send the message back to the client
        tunnel.send(message).await?;
    }
}

#[tokio::main]
async fn main() {
    let mut args = Arguments::parse();

    // Allow the auth token to be passed in an environment variable.
    if args.auth_token.is_none() {
        if let Some(env_token) = option_env!("PORTAL_TOKEN") {
            args.auth_token = Some(env_token.to_owned());
            tracing::debug!("using auth token in $PORTAL_TOKEN");
        }
    }

    tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .without_time()
        .init();

    let result = run_host(&args).await;
    if let Err(e) = result {
        tracing::info!("connection ended: {e:#}");
    }
}
