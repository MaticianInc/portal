use std::borrow::Cow;

use anyhow::{bail, Context};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use portal_client::PortalService;
use tokio::io::AsyncBufReadExt;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Arguments {
    /// Server base URL
    #[arg(long, default_value = "ws://localhost:3000")]
    server: String,

    /// The tunnel identifier
    #[arg(long, default_value = "1234")]
    tunnel_id: u64,

    /// Tunnel service auth token
    #[arg(long)]
    auth_token: Option<String>,
}

async fn run_client(args: &Arguments) -> anyhow::Result<()> {
    tracing::info!("running echo client");
    let service = PortalService::new(&args.server)?;
    let mut tunnel = service
        .tunnel_host(
            args.auth_token.as_deref().unwrap_or_default(),
            &args.tunnel_id.to_string(),
        )
        .await
        .context("failed to connect to service")?;

    let stdin = tokio::io::stdin();
    let stdin = tokio::io::BufReader::new(stdin);
    let mut stdin_lines = stdin.lines();

    loop {
        tokio::select! {
            stream_result = tunnel.next()  => {
                let message = stream_result
                .context("no message received")? // .next() returned None (shouldn't happen?)
                .context("error on websocket stream")?; // .next() returned Some(Err(...))

                let printable_message = match std::str::from_utf8(&message) {
                    Ok(text) => Cow::from(text),
                    Err(_) => Cow::from(format!("binary message ({} bytes)", message.len())),
                };
                tracing::info!("message from host: {printable_message}");
            },
            stdin_result = stdin_lines.next_line() => {
                match stdin_result {
                    Ok(Some(text)) => tunnel.send(text.into()).await.unwrap(),
                    _ => bail!("stdin closed"),
                }

            },
        }
    }
}

#[tokio::main]
async fn main() {
    let mut args = Arguments::parse();

    // Allow the auth token to be passed in an environment variable.
    if args.auth_token.is_none() {
        if let Some(env_token) = option_env!("PORTAL_TOKEN") {
            args.auth_token = Some(env_token.to_owned());
        }
    }

    tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .without_time()
        .init();

    let result = run_client(&args).await;
    if let Err(e) = result {
        tracing::info!("connection ended: {e:#}");
    }
}
